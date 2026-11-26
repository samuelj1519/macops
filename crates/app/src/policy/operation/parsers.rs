use crate::{
    domain::{Observation, Package, ProcessOutput, Provider},
    error::{Error, Result},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn exact_version(value: &str) -> Result<String> {
    if value.is_empty()
        || !value.as_bytes()[0].is_ascii_digit()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-+_".contains(&b))
    {
        return Err(Error::new(
            "exact_version_required",
            "请提供精确版本，不接受标签、范围或 shell 字符",
        ));
    }
    Ok(value.to_owned())
}

pub fn synchronous(p: &Package) -> bool {
    match &p.provider {
        Provider::Command { synchronous, .. } => *synchronous,
        Provider::Legacy { .. } => false,
        _ => true,
    }
}

pub fn parse_observation(p: &Package, out: &ProcessOutput) -> Result<Observation> {
    if out.uncertain {
        return Err(Error::new("query_unknown", "查询超时或输出超过限制"));
    }
    if matches!(p.provider, Provider::Brew { .. })
        && out.code == Some(1)
        && out.stdout.trim().is_empty()
        && out.stderr.trim().is_empty()
    {
        return Ok(Observation::Missing { evidence: None });
    }
    if out.code != Some(0) {
        return Err(Error::new(
            "query_failed",
            format!("{}: {}", p.id, out.stderr.trim()),
        ));
    }
    let version = match &p.provider {
        Provider::Command { .. } => {
            let observation: Observation = serde_json::from_str(&out.stdout)?;
            if matches!(
                observation,
                Observation::Service { .. }
                    | Observation::Present {
                        evidence: Some(_),
                        ..
                    }
                    | Observation::Missing { evidence: Some(_) }
            ) {
                return Err(Error::new("invalid_query_status", "软件查询不接受服务状态"));
            }
            if observation.version().is_some_and(str::is_empty) {
                return Err(Error::new("invalid_version", &p.id));
            }
            return Ok(observation);
        }
        Provider::Brew { package, .. } => brew_version(package, &out.stdout)?,
        Provider::Mise { package } => {
            let values: TargetMise = serde_json::from_str(&out.stdout)?;
            let candidates = values.versions(package)?;
            let active: Vec<_> = candidates.iter().filter(|v| v.active).collect();
            match active.as_slice() {
                [v] => Some(v.version.clone()),
                [] if candidates.len() == 1 => Some(candidates[0].version.clone()),
                [] if candidates.is_empty() => None,
                _ => return Err(Error::new("ambiguous_installation", &p.id)),
            }
        }
        Provider::Uv { package, .. } | Provider::Cargo { package, .. } => text_version(
            package,
            &out.stdout,
            matches!(p.provider, Provider::Cargo { .. }),
        )?,
        Provider::Bun { package } => bun_version(package, &out.stdout)?,
        _ => return Err(Error::new("unsupported_provider", &p.id)),
    };
    match version {
        Some(v) if v.is_empty() => Err(Error::new("invalid_version", &p.id)),
        Some(version) => Ok(Observation::Present {
            evidence: None,
            version,
        }),
        None => Ok(Observation::Missing { evidence: None }),
    }
}

pub fn bun_version(package: &str, text: &str) -> Result<Option<String>> {
    Ok(bun_inventory(text)?.1.get(package).cloned())
}

pub fn bun_inventory(text: &str) -> Result<(std::path::PathBuf, BTreeMap<String, String>)> {
    let lines: Vec<_> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let header = lines
        .first()
        .ok_or_else(|| Error::new("invalid_bun_inventory", "Bun 清单缺少表头"))?;
    let (root, count) = header
        .rsplit_once(" node_modules (")
        .ok_or_else(|| Error::new("invalid_bun_inventory", "Bun 表头格式已改变"))?;
    let count = count
        .strip_suffix(')')
        .ok_or_else(|| Error::new("invalid_bun_inventory", "Bun 表头计数不完整"))?;
    if !std::path::Path::new(root).is_absolute()
        || count.is_empty()
        || !count.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(Error::new(
            "invalid_bun_inventory",
            "Bun 表头路径或计数无效",
        ));
    }
    if (count == "0") != (lines.len() == 1) {
        return Err(Error::new("invalid_bun_inventory", "Bun 清单不完整"));
    }
    let mut names = std::collections::BTreeSet::new();
    let mut packages = BTreeMap::new();
    for (i, line) in lines.iter().enumerate().skip(1) {
        let prefix = if i == lines.len() - 1 {
            "└── "
        } else {
            "├── "
        };
        let row = line
            .strip_prefix(prefix)
            .ok_or_else(|| Error::new("invalid_bun_inventory", "Bun 条目层次或终结标记无效"))?;
        let (name, version) = row
            .rsplit_once('@')
            .ok_or_else(|| Error::new("invalid_bun_inventory", "Bun 条目缺少版本"))?;
        if !registry_name(name) || !names.insert(name.to_ascii_lowercase()) {
            return Err(Error::new("invalid_bun_inventory", "Bun 名称无效或重复"));
        }
        exact_version(version)?;
        packages.insert(name.into(), version.into());
    }
    Ok((
        crate::policy::paths::lexical(std::path::Path::new(root))?,
        packages,
    ))
}

pub fn registry_name(name: &str) -> bool {
    let valid = |s: &str| {
        !s.is_empty()
            && s.as_bytes()[0].is_ascii_alphanumeric()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    if let Some(scoped) = name.strip_prefix('@') {
        scoped
            .split_once('/')
            .is_some_and(|(scope, name)| valid(scope) && valid(name))
    } else {
        valid(name)
    }
}

pub fn text_version(package: &str, text: &str, cargo: bool) -> Result<Option<String>> {
    let mut found = None;
    let mut header_seen = false;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if line.starts_with(char::is_whitespace) {
            if !header_seen {
                return Err(Error::new(
                    "invalid_native_inventory",
                    "没有所属包的缩进条目",
                ));
            }
            continue;
        }
        let words: Vec<_> = line.split_whitespace().collect();
        if words.len() < 2 || !words[1].starts_with('v') || (cargo && !line.ends_with(':')) {
            return Err(Error::new("invalid_native_inventory", "无法识别原生包清单"));
        }
        let version = words[1].trim_start_matches('v').trim_end_matches(':');
        exact_version(version)?;
        header_seen = true;
        if words[0] == package {
            if found.is_some() {
                return Err(Error::new("ambiguous_installation", package));
            }
            found = Some(version.to_owned());
        }
    }
    Ok(found)
}

pub fn brew_version(package: &str, text: &str) -> Result<Option<String>> {
    let lines: Vec<_> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return Ok(None);
    }
    if lines.len() != 1 {
        return Err(Error::new("ambiguous_installation", package));
    }
    let fields: Vec<_> = lines[0].split_whitespace().collect();
    let name = package.rsplit('/').next().unwrap_or(package);
    if fields.len() != 2 || fields[0] != name || fields[1].is_empty() {
        return Err(Error::new("brew_identity_mismatch", package));
    }
    Ok(Some(fields[1].into()))
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum TargetMise {
    Direct(Vec<MiseVersion>),
    Named(BTreeMap<String, Vec<MiseVersion>>),
}

impl TargetMise {
    fn versions(&self, package: &str) -> Result<Vec<&MiseVersion>> {
        match self {
            Self::Direct(versions) => Ok(versions.iter().collect()),
            Self::Named(versions) => {
                if versions
                    .keys()
                    .any(|k| k != package && !package.ends_with(&format!(":{k}")))
                {
                    return Err(Error::new("mise_identity_mismatch", package));
                }
                Ok(versions.values().flatten().collect())
            }
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct CondaOutput {
    pub envs: Vec<String>,
}
#[derive(Deserialize, Serialize)]
pub struct ServiceOutput {
    pub name: String,
    pub running: bool,
    #[serde(default)]
    pub loaded: bool,
}

pub fn parse_conda(text: &str) -> Result<Value> {
    let data: CondaOutput = serde_json::from_str(text)?;
    Ok(serde_json::json!({"envs":data.envs}))
}
pub fn parse_service(text: &str, expected: &str) -> Result<ServiceOutput> {
    let mut records: Vec<ServiceOutput> = serde_json::from_str(text)
        .map_err(|_| Error::new("invalid_service_output", "服务查询格式或类型错误"))?;
    if records.len() != 1 || records[0].name != expected {
        return Err(Error::new("service_identity_mismatch", expected));
    }
    Ok(records.remove(0))
}

#[derive(Deserialize, Serialize)]
pub struct MiseVersion {
    pub version: String,
    #[serde(default)]
    pub active: bool,
}
#[derive(Deserialize, Serialize)]
pub struct BrewInventory {
    pub formulae: Vec<BrewFormula>,
    pub casks: Vec<BrewCask>,
}
#[derive(Deserialize, Serialize)]
pub struct BrewFormula {
    pub name: String,
    pub installed: Vec<BrewInstalled>,
}
#[derive(Deserialize, Serialize)]
pub struct BrewInstalled {
    pub version: String,
}
#[derive(Deserialize, Serialize)]
pub struct BrewCask {
    pub token: String,
    pub installed: Option<String>,
}
pub fn available(package: &Package, out: &ProcessOutput) -> Result<String> {
    if out.code != Some(0) || out.uncertain {
        return Err(Error::new("version_query_failed", &package.id));
    }
    let version = match &package.provider {
        Provider::Command { .. } => {
            serde_json::from_str::<crate::domain::Version>(&out.stdout)?.version
        }
        Provider::Npm { .. } | Provider::Bun { .. } => serde_json::from_str::<String>(&out.stdout)?,
        Provider::Mise { .. } => out.stdout.trim().to_owned(),
        Provider::Brew {
            package: requested,
            cask,
        } => {
            let data: Value = serde_json::from_str(&out.stdout)?;
            let rows = data
                .get(if *cask { "casks" } else { "formulae" })
                .and_then(Value::as_array)
                .ok_or_else(|| Error::new("invalid_version_output", &package.id))?;
            if rows.len() != 1 {
                return Err(Error::new("version_identity_mismatch", &package.id));
            }
            if rows[0]
                .get(if *cask { "token" } else { "name" })
                .and_then(Value::as_str)
                != Some(requested)
            {
                return Err(Error::new("version_identity_mismatch", &package.id));
            }
            let value = if *cask {
                rows[0].get("version")
            } else {
                rows[0].get("versions").and_then(|v| v.get("stable"))
            };
            let version = value
                .and_then(Value::as_str)
                .ok_or_else(|| Error::new("invalid_version_output", &package.id))?
                .to_owned();
            if *cask {
                version
            } else {
                let revision = rows[0].get("revision").map_or(Ok(0), |r| {
                    r.as_u64()
                        .ok_or_else(|| Error::new("invalid_formula_revision", &package.id))
                })?;
                if revision == 0 {
                    version
                } else {
                    format!("{version}_{revision}")
                }
            }
        }
        _ => return Err(Error::new("version_query_unsupported", &package.id)),
    };
    exact_version(&version)
}

pub fn tsv(text: &str, required: &[&str]) -> Result<Vec<BTreeMap<String, String>>> {
    let mut lines = text.lines();
    let header: Vec<_> = lines
        .next()
        .ok_or_else(|| Error::new("invalid_manifest", "清单缺少表头"))?
        .split('\t')
        .collect();
    if required.iter().any(|key| !header.contains(key))
        || header
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != header.len()
    {
        return Err(Error::new(
            "invalid_manifest",
            format!("必需列：{}", required.join(", ")),
        ));
    }
    let mut rows = vec![];
    for (i, line) in lines.enumerate() {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != header.len() {
            return Err(Error::new(
                "invalid_manifest",
                format!("第 {} 行列数错误", i + 2),
            ));
        }
        let row: BTreeMap<_, _> = header
            .iter()
            .zip(fields)
            .map(|(a, b)| ((*a).into(), b.into()))
            .collect();
        if required
            .iter()
            .any(|key| row.get(*key).is_none_or(String::is_empty))
        {
            return Err(Error::new(
                "invalid_manifest",
                format!("第 {} 行必需字段为空", i + 2),
            ));
        }
        rows.push(row);
    }
    Ok(rows)
}

pub fn app_plist(value: &plist::Value, bundle_id: &str, id: &str) -> Result<Observation> {
    let dictionary = value
        .as_dictionary()
        .ok_or_else(|| Error::new("invalid_plist", "Info.plist 必须是字典"))?;
    if dictionary
        .get("CFBundleIdentifier")
        .and_then(plist::Value::as_string)
        != Some(bundle_id)
    {
        return Err(Error::new("bundle_identity_mismatch", id));
    }
    let version = dictionary
        .get("CFBundleShortVersionString")
        .and_then(plist::Value::as_string)
        .ok_or_else(|| Error::new("invalid_version", id))?;
    Ok(Observation::Present {
        evidence: None,
        version: version.into(),
    })
}

pub fn manager_observation(output: &ProcessOutput) -> Result<Observation> {
    if output.code != Some(0) || output.uncertain {
        return Err(Error::new("query_failed", output.stderr.clone()));
    }
    let version = output.stdout.trim();
    if version.is_empty() {
        return Err(Error::new("invalid_version", "版本输出为空"));
    }
    Ok(Observation::Present {
        evidence: None,
        version: version.into(),
    })
}
pub fn managed_asset(value: &Value, selector: &str) -> Observation {
    value
        .get("assets")
        .and_then(|v| v.get(selector))
        .and_then(|v| v.get("revision"))
        .and_then(Value::as_str)
        .map_or_else(
            || Observation::Unknown {
                reason: "旧记录没有该项；不能据此推断未安装".into(),
            },
            |version| Observation::Present {
                evidence: None,
                version: version.into(),
            },
        )
}

pub fn discovery(manager: &str, out: &ProcessOutput) -> Result<Value> {
    if out.code != Some(0) || out.uncertain {
        return Err(Error::new(
            "discovery_failed",
            format!("{manager}: {}", out.stderr.trim()),
        ));
    }
    Ok(match manager {
        "conda" => parse_conda(&out.stdout)?,
        "mise" => {
            let data: BTreeMap<String, Vec<MiseVersion>> = serde_json::from_str(&out.stdout)?;
            serde_json::json!(data)
        }
        "brew" => {
            let data: BrewInventory = serde_json::from_str(&out.stdout)?;
            serde_json::json!(data)
        }
        _ => serde_json::json!({"native_output":out.stdout,"adopted":false}),
    })
}

#[cfg(test)]
#[path = "../../../tests/unit/parsers.rs"]
mod tests;
