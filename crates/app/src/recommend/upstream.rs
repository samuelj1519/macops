use super::candidate::{self as policy, Candidate, Evidence, Input};
use crate::{
    context::Context,
    error::{Error, Result},
    policy::public_source::{repository_id, repository_subject},
};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub(super) const REPOSITORY_SEARCH: &str = "https://api.github.com/search/repositories?";

#[derive(PartialEq, Eq, Debug)]
pub(super) struct RepositoryMatch {
    pub name: String,
    pub fork: bool,
}

fn search_url(name: &str) -> Option<String> {
    repository_id(&format!("github/{name}")).ok()?;
    Some(format!(
        "{REPOSITORY_SEARCH}q={name}%20in%3Aname%20is%3Apublic%20archived%3Afalse&per_page=8"
    ))
}

fn search_matches(name: &str, data: &Value) -> Vec<RepositoryMatch> {
    let mut seen = std::collections::BTreeSet::new();
    data["items"]
        .as_array()
        .into_iter()
        .flatten()
        .take(8)
        .filter_map(|entry| {
            let full_name = repository_id(entry["full_name"].as_str()?).ok()?;
            let (_, short) = full_name.split_once('/')?;
            let fork = entry["fork"].as_bool()?;
            if !name.eq_ignore_ascii_case(short)
                || entry["private"] != false
                || entry["archived"] != false
                || entry["disabled"] == true
                || !seen.insert(full_name.to_ascii_lowercase())
            {
                return None;
            }
            Some(RepositoryMatch {
                name: full_name.into(),
                fork,
            })
        })
        .collect()
}

pub(super) fn search(ctx: &Context, name: &str) -> Option<Result<Vec<RepositoryMatch>>> {
    let url = search_url(name)?;
    let deadline = Instant::now() + Duration::from_secs(ctx.timeout_secs.min(15));
    Some(super::model::fetch_public(&url, deadline).map(|data| search_matches(name, &data)))
}

struct RepositoryEvidence {
    repository: String,
    commit: crate::policy::source_git::ObjectId,
    default_branch: Option<String>,
    documents: Vec<Document>,
}

struct Document {
    path: String,
    content: String,
}

const CODEX_MARKETPLACE: &str = ".agents/plugins/marketplace.json";
const CODEX_PLUGIN: &str = ".codex-plugin/plugin.json";

fn documents_claude_market(evidence: &RepositoryEvidence) -> bool {
    evidence.documents.iter().any(|doc| {
        doc.content.lines().any(|line| {
            let mut words = line.split_ascii_whitespace();
            words
                .by_ref()
                .take(4)
                .eq(["claude", "plugin", "marketplace", "add"])
                && words
                    .next()
                    .and_then(repository_subject)
                    .is_some_and(|repo| repo.eq_ignore_ascii_case(&evidence.repository))
                && words.next().is_none()
        })
    })
}

fn claude_marketplace(
    evidence: &RepositoryEvidence,
    fetch: &mut impl FnMut(&str) -> Result<Value>,
) -> Option<crate::agent::claude_install_source::Offer> {
    use crate::agent::claude_install_source::{MARKET, Offer, PLUGIN};
    if !documents_claude_market(evidence) {
        return None;
    }
    let base = format!("https://api.github.com/repos/{}", evidence.repository);
    let market = public_file(fetch, &base, MARKET, &evidence.commit)?;
    let plugin = public_file(fetch, &base, PLUGIN, &evidence.commit)?;
    let offer = Offer::identify(
        &evidence.repository,
        &evidence.commit,
        market.content.as_bytes(),
        plugin.content.as_bytes(),
    )
    .ok()?;
    let id = offer.native_id();
    evidence
        .documents
        .iter()
        .any(|doc| {
            doc.content.lines().any(|line| {
                let mut words = line.split_ascii_whitespace();
                words
                    .by_ref()
                    .take(4)
                    .eq(["claude", "plugin", "install", id.as_str()])
                    && matches!(
                        (words.next(), words.next(), words.next()),
                        (None, None, None) | (Some("--scope"), Some("user"), None)
                    )
            })
        })
        .then_some(offer)
}

fn public_file(
    fetch: &mut impl FnMut(&str) -> Result<Value>,
    base: &str,
    path: &str,
    commit: &crate::policy::source_git::ObjectId,
) -> Option<Document> {
    let value = fetch(&format!("{base}/contents/{path}?ref={}", commit.as_str())).ok()?;
    document(&value, Some(path))
}

fn documents_codex_market(evidence: &RepositoryEvidence, doc: &Document) -> bool {
    doc.content.lines().any(|line| {
        let mut words = line.split_ascii_whitespace();
        words
            .by_ref()
            .take(4)
            .eq(["codex", "plugin", "marketplace", "add"])
            && words
                .next()
                .and_then(repository_subject)
                .is_some_and(|source| source.eq_ignore_ascii_case(&evidence.repository))
            && match (words.next(), words.next(), words.next()) {
                (None, None, None) => true,
                (Some("--ref"), Some(reference), None) => {
                    reference == evidence.commit.as_str()
                        || Some(reference) == evidence.default_branch.as_deref()
                }
                _ => false,
            }
    })
}

fn documents_codex_plugin(doc: &Document, id: &str) -> bool {
    doc.content.lines().any(|line| {
        line.split_ascii_whitespace()
            .eq(["codex", "plugin", "add", id])
    })
}

fn codex_marketplace(
    evidence: &RepositoryEvidence,
    fetch: &mut impl FnMut(&str) -> Result<Value>,
) -> Option<Value> {
    use crate::policy::source_git::{ObjectId, Repository};
    let repo = &evidence.repository;
    let market_instructions: Vec<_> = evidence
        .documents
        .iter()
        .filter(|doc| documents_codex_market(evidence, doc))
        .collect();
    if market_instructions.is_empty() {
        return None;
    }
    let base = format!("https://api.github.com/repos/{repo}");
    let market_doc = public_file(fetch, &base, CODEX_MARKETPLACE, &evidence.commit)?;
    let market = crate::policy::json::parse(market_doc.content.as_bytes()).ok()?;
    let market_name = market["name"]
        .as_str()
        .filter(|name| crate::agent::native_plugins::valid_name(name))?;
    let candidates = market["plugins"]
        .as_array()
        .filter(|rows| rows.len() <= 512)?;
    let mut matching = candidates.iter().filter_map(|item| {
        let name = item["name"]
            .as_str()
            .filter(|name| crate::agent::native_plugins::valid_name(name))?;
        let id = format!("{name}@{market_name}");
        let instructions = market_instructions
            .iter()
            .find(|doc| documents_codex_plugin(doc, &id))?;
        Some((name, &item["source"], *instructions))
    });
    let (plugin_name, source, instructions) = matching.next()?;
    if matching.next().is_some()
        || source["source"] != "url"
        || !source["path"].is_null()
        || !source["sha"].is_null()
    {
        return None;
    }
    let url = source["url"].as_str()?;
    let canonical = Repository::parse(repo).ok()?.url();
    if url != canonical && canonical.strip_suffix(".git") != Some(url) {
        return None;
    }
    let reference = source["ref"]
        .as_str()
        .filter(|value| crate::agent::native_plugins::valid_name(value) && !value.contains(".."))?;
    let commit = if let Ok(commit) = ObjectId::parse(reference) {
        commit
    } else {
        let resolved = fetch(&format!("{base}/commits/{reference}")).ok()?;
        ObjectId::parse(resolved["sha"].as_str()?).ok()?
    };
    let plugin_doc = public_file(fetch, &base, CODEX_PLUGIN, &commit)?;
    let plugin = crate::policy::json::parse(plugin_doc.content.as_bytes()).ok()?;
    if plugin["name"].as_str() != Some(plugin_name) {
        return None;
    }
    let version = plugin["version"]
        .as_str()
        .filter(|version| crate::agent::native_plugins::valid_name(version))?;
    let sha256 = |doc: &Document| format!("{:x}", Sha256::digest(doc.content.as_bytes()));
    Some(json!({
        "marketplace":market_name,"plugin":plugin_name,"declaredVersion":version,
        "repository":repo,"payloadRef":reference,
        "marketplaceCommit":evidence.commit.as_str(),"payloadCommit":commit.as_str(),
        "marketplaceSha256":sha256(&market_doc),"pluginManifestSha256":sha256(&plugin_doc),
        "instructions":instructions.path,"marketplaceManifest":CODEX_MARKETPLACE,
        "pluginManifest":CODEX_PLUGIN,
        "status":if commit == evidence.commit {"same-commit-observed"} else {"divergent-commits"}
    }))
}

fn document(value: &Value, expected_path: Option<&str>) -> Option<Document> {
    let path = value["path"].as_str()?;
    if value["type"] != "file"
        || value["encoding"] != "base64"
        || path == "."
        || expected_path.is_some_and(|expected| expected != path)
        || crate::policy::source_git::RelativePath::parse(path).is_err()
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-._".contains(&b))
    {
        return None;
    }
    let encoded = value["content"].as_str()?.replace('\n', "");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    Some(Document {
        path: path.into(),
        content: String::from_utf8(bytes).ok()?,
    })
}

fn repository_evidence(
    repo: &str,
    mut fetch: impl FnMut(&str) -> Result<Value>,
) -> Result<RepositoryEvidence> {
    let repo = repository_id(repo)?;
    let metadata = fetch(&format!("https://api.github.com/repos/{repo}"))?;
    let repository = metadata["full_name"].as_str().ok_or_else(unavailable)?;
    if metadata["private"] != false || !repository.eq_ignore_ascii_case(repo) {
        return Err(unavailable());
    }
    let base = format!("https://api.github.com/repos/{repository}");
    let latest = fetch(&format!("{base}/commits?per_page=1"))?;
    let commit = crate::policy::source_git::ObjectId::parse(
        latest[0]["sha"].as_str().ok_or_else(unavailable)?,
    )
    .map_err(|_| unavailable())?;
    let reference = commit.as_str();
    let readme = fetch(&format!("{base}/readme?ref={reference}"))
        .ok()
        .and_then(|result| document(&result, None));
    let install = fetch(&format!("{base}/contents/INSTALL.md?ref={reference}"))
        .ok()
        .and_then(|result| document(&result, Some("INSTALL.md")));
    Ok(RepositoryEvidence {
        repository: repository.into(),
        commit,
        default_branch: metadata["default_branch"].as_str().map(str::to_owned),
        documents: readme.into_iter().chain(install).collect(),
    })
}

fn install_word(value: &str) -> bool {
    value.len() <= 100
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn matching_install_source(source: &str, separator: char, repository: &str) -> bool {
    let (name, revision) = source
        .split_once(separator)
        .map_or((source, None), |(name, revision)| (name, Some(revision)));
    revision.is_none_or(install_word)
        && repository_subject(name).is_some_and(|name| name.eq_ignore_ascii_case(repository))
}

fn skills_install_command(line: &str, repository: &str) -> bool {
    let mut words = line.split_ascii_whitespace();
    if words.next() != Some("npx") {
        return false;
    }
    let Some(mut package) = words.next() else {
        return false;
    };
    if matches!(package, "--yes" | "-y") {
        let Some(next) = words.next() else {
            return false;
        };
        package = next;
    }
    if !(package == "skills" || package.strip_prefix("skills@").is_some_and(install_word))
        || words.next() != Some("add")
        || !words
            .next()
            .is_some_and(|source| matching_install_source(source, '@', repository))
    {
        return false;
    }
    while let Some(option) = words.next() {
        match option {
            "--yes" | "-y" | "--copy" | "--global" | "-g" => {}
            "--agent" if words.next().is_some_and(install_word) => {}
            "--skill"
                if words
                    .next()
                    .is_some_and(|name| matches!(name, "'*'" | "\"*\"") || install_word(name)) => {}
            _ => return false,
        }
    }
    true
}

fn claude_install_command(words: &[&str]) -> bool {
    matches!(words, ["claude", "plugin", "install", id] | ["claude", "plugin", "install", id, "--scope", "user" | "project" | "local"]
        if id.split_once('@').is_some_and(|(name, market)| install_word(name) && install_word(market)))
}

fn markdown_fence(line: &str) -> Option<(u8, usize, &str)> {
    let marker = *line.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = line.bytes().take_while(|byte| *byte == marker).count();
    (length >= 3).then(|| (marker, length, line[length..].trim()))
}

fn documented_install_options(evidence: &RepositoryEvidence) -> Vec<Value> {
    let mut skills = None;
    let mut claude = None;
    for (index, document) in evidence.documents.iter().enumerate() {
        let mut block = None;
        let mut market = None;
        for line in document.content.lines().map(str::trim) {
            if let Some((marker, length, language)) = markdown_fence(line) {
                match block {
                    None => {
                        block = Some((
                            marker,
                            length,
                            matches!(language, "sh" | "bash" | "zsh" | "shell"),
                        ));
                    }
                    Some((open, minimum, _))
                        if open == marker && length >= minimum && language.is_empty() =>
                    {
                        block = None;
                    }
                    _ => {}
                }
                market = None;
                continue;
            }
            if !block.is_some_and(|(_, _, shell)| shell) || line.is_empty() {
                continue;
            }
            let preceding_market = market.take();
            if line.len() > 512 {
                continue;
            }
            let words: Vec<_> = line.split_ascii_whitespace().collect();
            if skills.is_none() && skills_install_command(line, &evidence.repository) {
                skills = Some(
                    json!({"manager":"Skills CLI","document":index,"commands":[words.join(" ")]}),
                );
            }
            if claude.is_none() {
                if let ["claude", "plugin", "marketplace", "add", source] = words.as_slice() {
                    if matching_install_source(source, '#', &evidence.repository) {
                        market = Some(words.join(" "));
                    }
                } else if claude_install_command(&words)
                    && let Some(preceding) = preceding_market
                {
                    claude = Some(
                        json!({"manager":"Claude Code","document":index,"commands":[preceding,words.join(" ")]}),
                    );
                }
            }
        }
    }
    [skills, claude].into_iter().flatten().collect()
}

pub(super) fn show_repository(
    ctx: &Context,
    repo: &str,
    dry_run: bool,
    interactive: bool,
) -> Result<Value> {
    if dry_run {
        return show(ctx, repo, repo, true);
    }
    let deadline = Instant::now() + Duration::from_secs(ctx.timeout_secs.min(30));
    Ok(repository_review(ctx, repo, interactive, |url| {
        super::model::fetch_public(url, deadline)
    }))
}

fn repository_review(
    ctx: &Context,
    repo: &str,
    interactive: bool,
    mut fetch: impl FnMut(&str) -> Result<Value>,
) -> Value {
    match repository_evidence(repo, &mut fetch) {
        Ok(evidence) => {
            let codex_marketplace = codex_marketplace(&evidence, &mut fetch);
            let claude_marketplace = claude_marketplace(&evidence, &mut fetch);
            let model = match interactive
                .then(|| super::setup::read(&ctx.config))
                .transpose()
            {
                Ok(Some(Some(profile))) if profile.enabled => {
                    let documents: Vec<_> = evidence
                        .documents
                        .iter()
                        .map(|doc| (doc.path.as_str(), doc.content.as_str()))
                        .collect();
                    super::model::suggest(ctx, &profile, &evidence.repository, &documents)
                }
                Ok(_) => json!({"status":"local-only","candidates":[]}),
                Err(_) => json!({"status":"profile-unavailable","candidates":[]}),
            };
            let commit = evidence.commit.as_str();
            let documents: Vec<_> = evidence.documents.iter().map(|doc| json!({
                "path": doc.path,
                "url": format!("https://github.com/{}/blob/{commit}/{}", evidence.repository, doc.path),
                "sha256": format!("{:x}", Sha256::digest(doc.content.as_bytes())),
            })).collect();
            let mut data = json!({
                "query":repo,"repository":evidence.repository,"commit":commit,
                "documents":documents,"model":model,
                "evidenceStatus":if documents.is_empty() {"documents-unavailable"} else {"public-commit"},
                "canExecute":false,"persistentWrites":false
            });
            let manual = documented_install_options(&evidence);
            if !manual.is_empty() {
                data["manualInstall"] = json!(manual);
            }
            if let Some(market) = codex_marketplace {
                data["codexMarketplace"] = market;
            }
            if let Some(market) = claude_marketplace {
                data["claudeMarketplace"] = json!(market);
            }
            data
        }
        Err(error) => json!({
            "query":repo,"repository":repo,"evidenceStatus":"source-unavailable",
            "reason":error.code,"canExecute":false,"persistentWrites":false
        }),
    }
}

pub(super) fn show(ctx: &Context, query: &str, repo: &str, dry_run: bool) -> Result<Value> {
    let repo = repository_id(repo)?;
    if dry_run {
        return Ok(json!({
            "query":query,"repository":repo,"evidenceStatus":"dry-run",
            "persistentWrites":false,"canExecute":false
        }));
    }
    let deadline = Instant::now() + Duration::from_secs(ctx.timeout_secs.min(15));
    let mut fetch = |url: &str| super::model::fetch_public(url, deadline);
    match inspect(query, repo, &mut fetch) {
        Ok(mut report) => {
            attach_marketplaces(repo, &mut fetch, &mut report);
            Ok(report)
        }
        Err(error) => Ok(json!({
            "query":query,"repository":repo,"evidenceStatus":"source-unavailable",
            "reason":error.code,"persistentWrites":false,"canExecute":false
        })),
    }
}

fn attach_marketplaces(
    repo: &str,
    fetch: &mut impl FnMut(&str) -> Result<Value>,
    report: &mut Value,
) {
    let Ok(evidence) = repository_evidence(repo, &mut *fetch) else {
        return;
    };
    if let Some(market) = codex_marketplace(&evidence, &mut *fetch) {
        report["codexMarketplace"] = market;
    }
    if let Some(offer) = claude_marketplace(&evidence, &mut *fetch) {
        report["claudeMarketplace"] = json!(offer);
    }
}

fn unavailable() -> Error {
    Error::attention(
        "recommend_source_unverified",
        "无法核对所选公开仓库及其 README",
    )
}

fn inspect(
    query: &str,
    repository: &str,
    mut fetch: impl FnMut(&str) -> Result<Value>,
) -> Result<Value> {
    let repo = repository_id(repository)?;
    let metadata = fetch(&format!("https://api.github.com/repos/{repo}"))?;
    let published = metadata["full_name"].as_str().ok_or_else(unavailable)?;
    if metadata["private"] != false || !published.eq_ignore_ascii_case(repo) {
        return Err(unavailable());
    }
    let readme = fetch(&format!("https://api.github.com/repos/{repo}/readme"))?;
    let url = readme["html_url"].as_str().ok_or_else(unavailable)?;
    if !url.starts_with(&format!("https://github.com/{published}/blob/"))
        || readme["encoding"] != "base64"
    {
        return Err(unavailable());
    }
    let encoded = readme["content"].as_str().ok_or_else(unavailable)?;
    let encoded = encoded.replace('\n', "");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| unavailable())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| unavailable())?;
    let excerpt = relevant_lines(text);
    let evidence = Input {
        schema_version: 1,
        sources: vec![Evidence {
            subject: query.into(),
            url: url.into(),
            excerpt,
        }],
    };
    let result = policy::analyze(query, &evidence)?;
    let mut claims = BTreeMap::new();
    let comparisons = result
        .findings
        .iter()
        .enumerate()
        .filter_map(|(index, finding)| {
            registry_url(&finding.candidate).map(|(url, scope)| (index, url, scope))
        })
        .take(6)
        .map(|(index, url, scope)| {
            let metadata = claims
                .entry(url.clone())
                .or_insert_with(|| fetch(&url).ok());
            let inspected = match metadata.as_ref() {
                Some(value)
                    if publishes_from(&result.findings[index].candidate, published, value) =>
                {
                    "upstream-matched"
                }
                Some(_) => "upstream-unverified",
                None => "source-unavailable",
            };
            json!({"candidate":index,"url":url,"evidenceScope":scope,"status":inspected})
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "query":query,
        "repository":published,
        "evidenceStatus":"published-by-selected-repository",
        "model":{"status":"remote-evidence-local-only"},
        "readmeSha256":format!("{:x}", Sha256::digest(&bytes)),
        "analysis":result,
        "publisherChecks":comparisons,
        "canExecute":false,
        "persistentWrites":false,
    }))
}

fn registry_url(candidate: &Candidate) -> Option<(String, &'static str)> {
    match candidate {
        Candidate::Npm { name, version, .. } => {
            let (version, scope) = match version.as_deref() {
                None | Some("latest") => ("latest", "current-release"),
                Some(version)
                    if crate::policy::operation::manifest::concrete_npm_version(version) =>
                {
                    (version, "requested-version")
                }
                _ => return None,
            };
            Some((
                format!(
                    "https://registry.npmjs.org/{}/{version}",
                    name.replace('/', "%2F")
                ),
                scope,
            ))
        }
        Candidate::Brew { name, cask: false } => core_formula(name).map(|name| {
            (
                format!("https://formulae.brew.sh/api/formula/{name}.json"),
                "current-formula",
            )
        }),
        _ => None,
    }
}

fn core_formula(name: &str) -> Option<&str> {
    let name = name.strip_prefix("homebrew/core/").unwrap_or(name);
    (!name.contains('/')).then_some(name)
}

fn publishes_from(candidate: &Candidate, repository: &str, claim: &Value) -> bool {
    match candidate {
        Candidate::Npm { name, version, .. } => {
            let upstream = claim["repository"]
                .as_str()
                .or_else(|| claim["repository"]["url"].as_str());
            claim["name"] == *name
                && claim["version"]
                    .as_str()
                    .is_some_and(crate::policy::operation::manifest::valid_version)
                && version
                    .as_deref()
                    .is_none_or(|expected| expected == "latest" || claim["version"] == expected)
                && upstream.is_some_and(|url| same_repository(url, repository))
        }
        Candidate::Brew { name, cask: false } => {
            core_formula(name).is_some_and(|name| claim["name"] == name)
                && claim["tap"] == "homebrew/core"
                && (claim["homepage"]
                    .as_str()
                    .is_some_and(|url| same_repository(url, repository))
                    || claim["urls"]["stable"]["url"]
                        .as_str()
                        .is_some_and(|url| repository_release(url, repository)))
        }
        _ => false,
    }
}

fn same_repository(url: &str, repo: &str) -> bool {
    let url = url.strip_prefix("git+").unwrap_or(url);
    [
        "https://github.com/",
        "git://github.com/",
        "ssh://git@github.com/",
    ]
    .iter()
    .any(|prefix| {
        url.strip_prefix(prefix).is_some_and(|path| {
            let path = path.trim_end_matches('/');
            path.strip_suffix(".git")
                .unwrap_or(path)
                .eq_ignore_ascii_case(repo)
        })
    })
}

fn repository_release(url: &str, repo: &str) -> bool {
    let Some(path) = url.strip_prefix("https://github.com/") else {
        return false;
    };
    let Some((owner, remainder)) = path.split_once('/') else {
        return false;
    };
    let Some((project, release)) = remainder.split_once('/') else {
        return false;
    };
    format!("{owner}/{project}").eq_ignore_ascii_case(repo)
        && release
            .strip_prefix("archive/")
            .or_else(|| release.strip_prefix("releases/download/"))
            .is_some_and(|tail| {
                tail.split('/').all(|part| {
                    !matches!(part, "" | "." | "..")
                        && part
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"._-+".contains(&byte))
                })
            })
}

fn relevant_lines(text: &str) -> String {
    let mut excerpt = String::new();
    let mut examples = 0;
    for line in text.lines() {
        let line = line.trim().trim_matches('`');
        let line = line.strip_prefix("$ ").unwrap_or(line);
        let Some(head) = line.split_whitespace().next() else {
            continue;
        };
        if !matches!(head, "brew" | "npm" | "npx" | "mise") || line.len() > 1024 {
            continue;
        }
        if examples == 128 || excerpt.len() + line.len() + 1 > 8 * 1024 {
            break;
        }
        excerpt.push_str(line);
        excerpt.push('\n');
        examples += 1;
    }
    excerpt
}

#[cfg(test)]
#[path = "../../tests/unit/recommend_upstream.rs"]
mod tests;
