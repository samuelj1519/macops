use crate::{
    context::Context,
    error::{Error, Result},
    policy::recommend::{self as policy, Candidate, Evidence, Input},
};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

fn repository_id(value: &str) -> Result<&str> {
    let part = |name: &str| {
        name.len() <= 100
            && name
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    if value
        .split_once('/')
        .is_some_and(|(owner, repo)| part(owner) && part(repo))
    {
        Ok(value)
    } else {
        Err(Error::new(
            "recommend_repository_invalid",
            "GitHub 仓库需要 OWNER/REPO",
        ))
    }
}

pub fn show(ctx: &Context, query: &str, repo: &str, dry_run: bool) -> Result<Value> {
    let repo = repository_id(repo)?;
    if dry_run {
        return Ok(json!({
            "query":query,"repository":repo,"evidenceStatus":"dry-run",
            "persistentWrites":false,"canExecute":false
        }));
    }
    let deadline = Instant::now() + Duration::from_secs(ctx.timeout_secs.min(15));
    match inspect(query, repo, |url| {
        crate::recommend_model::fetch_public(url, deadline)
    }) {
        Ok(value) => Ok(value),
        Err(error) => Ok(json!({
            "query":query,"repository":repo,"evidenceStatus":"source-unavailable",
            "reason":error.code,"persistentWrites":false,"canExecute":false
        })),
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
                Some(version) if crate::policy::manifest::concrete_npm_version(version) => {
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
                    .is_some_and(crate::policy::manifest::valid_version)
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
#[path = "../tests/unit/recommend_upstream.rs"]
mod tests;
