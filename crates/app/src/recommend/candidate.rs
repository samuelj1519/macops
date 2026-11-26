use crate::error::{Error, Result};
use crate::policy::public_source::domain_link;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub url: String,
    pub subject: String,
    pub excerpt: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub schema_version: u32,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub sources: Vec<Evidence>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    Project,
    Global,
    Invocation,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "channel", rename_all = "kebab-case")]
pub enum Candidate {
    Brew {
        name: String,
        cask: bool,
    },
    Npm {
        name: String,
        version: Option<String>,
        scope: Scope,
        development: bool,
    },
    Npx {
        name: String,
        scope: Scope,
    },
    Mise {
        name: String,
        request: Option<String>,
        selection: Option<Scope>,
    },
    Skills {
        repository: String,
        member: Option<String>,
        agent: Option<String>,
        scope: Scope,
        copy: bool,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub candidate: Candidate,
    pub evidence_urls: BTreeSet<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub query: String,
    pub findings: Vec<Finding>,
    pub unknown_command_examples: usize,
    pub unrelated_sources: usize,
}

fn invalid() -> Error {
    Error::new(
        "recommend_evidence_invalid",
        "安装证据的主体、来源 URL、内容或预算无效",
    )
}

pub fn query_name(query: &str) -> Result<&str> {
    let query = query.trim();
    if query.is_empty() || query.len() > 128 || query.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(query)
}

pub fn analyze(query: &str, input: &Input) -> Result<Analysis> {
    let query = query_name(query)?;
    if input.schema_version != 1 || input.sources.is_empty() || input.sources.len() > 16 {
        return Err(invalid());
    }
    let mut findings: BTreeMap<Candidate, BTreeSet<String>> = BTreeMap::new();
    let mut unrelated_sources = 0;
    let mut unknown_command_examples = 0;
    for evidence in &input.sources {
        if !domain_link(&evidence.url)
            || evidence.excerpt.len() > 8 * 1024
            || evidence.excerpt.lines().count() > 128
        {
            return Err(invalid());
        }
        if !query_name(&evidence.subject)?.eq_ignore_ascii_case(query) {
            unrelated_sources += 1;
            continue;
        }
        for line in evidence.excerpt.lines() {
            let line = line.trim().trim_matches('`');
            let Some(head) = line.split_whitespace().next() else {
                continue;
            };
            if !matches!(head, "brew" | "npm" | "npx" | "mise") {
                continue;
            }
            if let Some(candidate) = parse(line) {
                findings
                    .entry(candidate)
                    .or_default()
                    .insert(evidence.url.clone());
            } else {
                unknown_command_examples += 1;
            }
        }
    }
    Ok(Analysis {
        query: query.to_owned(),
        findings: findings
            .into_iter()
            .map(|(candidate, evidence_urls)| Finding {
                candidate,
                evidence_urls,
            })
            .collect(),
        unknown_command_examples,
        unrelated_sources,
    })
}

fn name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && value.len() <= 128
        && bytes.all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn versioned_package(spec: &str) -> Option<(&str, Option<&str>)> {
    let rest = spec.strip_prefix('@').unwrap_or(spec);
    let split = rest.split_once('@').map(|(name, version)| {
        let length = name.len() + usize::from(spec.starts_with('@'));
        (&spec[..length], version)
    });
    let (package, version) = if let Some((package, version)) = split {
        if !name(version) {
            return None;
        }
        (package, Some(version))
    } else {
        (spec, None)
    };
    crate::policy::operation::parsers::registry_name(package).then_some((package, version))
}

fn parse(line: &str) -> Option<Candidate> {
    let words: Vec<_> = line.split_ascii_whitespace().collect();
    match words.as_slice() {
        ["brew", "install", package] | ["brew", "install", "--formula", package] => {
            brew(package, false)
        }
        ["brew", "install", "--cask", package] => brew(package, true),
        ["npm", "install" | "i", spec] => npm(spec, Scope::Project, false),
        ["npm", "install" | "i", "-D" | "--save-dev", spec]
        | ["npm", "install" | "i", spec, "-D" | "--save-dev"] => npm(spec, Scope::Project, true),
        ["npm", "install" | "i", "-g" | "--global", spec]
        | ["npm", "install" | "i", spec, "-g" | "--global"] => npm(spec, Scope::Global, false),
        ["npx", "skills", "add", repo, options @ ..] => skills(repo, options),
        ["npx", spec] => versioned_package(spec).map(|_| Candidate::Npx {
            name: (*spec).into(),
            scope: Scope::Invocation,
        }),
        ["mise", "install", spec] => mise(spec, None),
        ["mise", "use", spec] => mise(spec, Some(Scope::Project)),
        ["mise", "use", "-g" | "--global", spec] => mise(spec, Some(Scope::Global)),
        _ => None,
    }
}

fn brew(package: &str, cask: bool) -> Option<Candidate> {
    let parts: Vec<_> = package.split('/').collect();
    (matches!(parts.len(), 1 | 3) && parts.iter().all(|part| name(part))).then(|| Candidate::Brew {
        name: package.into(),
        cask,
    })
}

fn npm(spec: &str, scope: Scope, development: bool) -> Option<Candidate> {
    let (package, version) = versioned_package(spec)?;
    Some(Candidate::Npm {
        name: package.into(),
        version: version.map(str::to_owned),
        scope,
        development,
    })
}

fn mise(spec: &str, selection: Option<Scope>) -> Option<Candidate> {
    let (tool, request) = spec
        .split_once('@')
        .map_or((spec, None), |(n, v)| (n, Some(v)));
    (name(tool) && request.is_none_or(name)).then(|| Candidate::Mise {
        name: tool.into(),
        request: request.map(str::to_owned),
        selection,
    })
}

fn skills(repository: &str, mut options: &[&str]) -> Option<Candidate> {
    let (owner, repo) = repository.split_once('/')?;
    if !name(owner) || !name(repo) {
        return None;
    }
    let mut member = None;
    let mut agent = None;
    let mut scope = Scope::Project;
    let mut copy = false;
    while !options.is_empty() {
        options = match options {
            ["--skill" | "-s", selection, rest @ ..] if member.is_none() && name(selection) => {
                member = Some((*selection).into());
                rest
            }
            ["--agent" | "-a", selection, rest @ ..] if agent.is_none() && name(selection) => {
                agent = Some((*selection).into());
                rest
            }
            ["--global" | "-g", rest @ ..] if scope == Scope::Project => {
                scope = Scope::Global;
                rest
            }
            ["--copy", rest @ ..] if !copy => {
                copy = true;
                rest
            }
            _ => return None,
        };
    }
    Some(Candidate::Skills {
        repository: repository.into(),
        member,
        agent,
        scope,
        copy,
    })
}

#[cfg(test)]
#[path = "../../tests/unit/recommend.rs"]
mod tests;

pub const PUBLIC_SNAPSHOT_DATE: &str = "2026-11-21";

#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct PublicAnalysis(Analysis);

impl PublicAnalysis {
    pub fn facts(&self) -> &Analysis {
        &self.0
    }
}

pub fn public_analysis(query: &str) -> Result<Option<PublicAnalysis>> {
    public_snapshot(query)
        .map(|evidence| analyze(query, &evidence).map(PublicAnalysis))
        .transpose()
}

fn public_snapshot(query: &str) -> Option<Input> {
    let (subject, url, commands) = if query.eq_ignore_ascii_case("jq") {
        ("jq", "https://jqlang.org/download/", "brew install jq")
    } else if query.eq_ignore_ascii_case("TypeScript") {
        (
            "TypeScript",
            "https://www.typescriptlang.org/download/",
            "npm install typescript --save-dev\nnpm install -g typescript\nnpx tsc",
        )
    } else if query.eq_ignore_ascii_case("node") {
        (
            "node",
            "https://mise.jdx.dev/lang/node.html",
            "mise use node@26\nmise use -g node@26",
        )
    } else if query.eq_ignore_ascii_case("vercel-labs/agent-skills") {
        (
            "vercel-labs/agent-skills",
            "https://github.com/vercel-labs/skills/blob/main/src/cli.ts",
            "npx skills add vercel-labs/agent-skills\nnpx skills add vercel-labs/agent-skills -g",
        )
    } else {
        return None;
    };
    Some(Input {
        schema_version: 1,
        sources: vec![Evidence {
            url: url.into(),
            subject: subject.into(),
            excerpt: commands.into(),
        }],
    })
}
