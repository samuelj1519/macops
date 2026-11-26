use crate::{
    domain::resource::{Coverage, CoverageState},
    error::{Error, Result},
};
use serde::{
    Deserialize,
    de::{IgnoredAny, MapAccess, Visitor},
};
use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceName {
    pub target: String,
    pub template: bool,
    pub symlink: bool,
}
const ATTRIBUTES: &[&str] = &[
    "encrypted_",
    "private_",
    "readonly_",
    "empty_",
    "executable_",
    "dot_",
    "literal_",
    "create_",
    "remove_",
    "run_",
    "symlink_",
    "modify_",
    "external_",
    "exact_",
];

pub fn source_name(name: &str, directory: bool) -> Result<Option<SourceName>> {
    if name.starts_with('.') {
        return Ok(None);
    }
    if name.is_empty() || name.contains(['/', '\0']) {
        return Err(Error::new("invalid_source_name", "源文件名无法映射"));
    }
    let (mut value, symlink) = if !directory && name.starts_with("symlink_") {
        (&name["symlink_".len()..], true)
    } else {
        let mut value = name;
        for prefix in if directory {
            &["private_", "readonly_"][..]
        } else {
            &["private_", "readonly_", "executable_"][..]
        } {
            value = value.strip_prefix(prefix).unwrap_or(value);
        }
        (value, false)
    };
    let stripped = if directory {
        None
    } else {
        value.strip_suffix(".tmpl")
    };
    let template = stripped.is_some();
    if !directory {
        if value.ends_with(".literal") {
            return Err(unsupported());
        }
        if let Some(stripped) = stripped {
            value = stripped;
        }
        if value.ends_with(".literal") {
            return Err(unsupported());
        }
    }
    let target = if let Some(rest) = value.strip_prefix("dot_") {
        format!(".{rest}")
    } else {
        if ATTRIBUTES.iter().any(|prefix| value.starts_with(prefix)) {
            return Err(unsupported());
        }
        value.into()
    };
    if target.is_empty() || matches!(target.as_str(), "." | "..") {
        return Err(Error::new("invalid_source_name", "源属性生成无效目标组件"));
    }
    Ok(Some(SourceName {
        target,
        template,
        symlink,
    }))
}
fn unsupported() -> Error {
    Error::attention("unsupported_source_attribute", "源属性不在静态候选子集")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootFields {
    pub source: Option<String>,
    pub destination: Option<String>,
}
fn root_key(key: &str) -> Result<Option<bool>> {
    if !key.is_ascii() {
        return Err(Error::attention(
            "unsupported_config_key",
            "静态路径读取只接受ASCII配置字段名",
        ));
    }
    Ok(match key.to_ascii_lowercase().as_str() {
        "sourcedir" => Some(true),
        "destdir" => Some(false),
        _ => None,
    })
}
fn set_field(fields: &mut RootFields, source: bool, value: String) -> Result<()> {
    let field = if source {
        &mut fields.source
    } else {
        &mut fields.destination
    };
    if field.replace(value).is_some() {
        return Err(Error::new("duplicate_config_root", "配置根字段有同义冲突"));
    }
    Ok(())
}
impl<'de> Deserialize<'de> for RootFields {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Fields;
        impl<'de> Visitor<'de> for Fields {
            type Value = RootFields;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object containing static path declarations")
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
                let mut fields = RootFields::default();
                while let Some(key) = map.next_key::<String>()? {
                    if let Some(source) = root_key(&key)
                        .map_err(|_| serde::de::Error::custom("unsupported config key"))?
                    {
                        set_field(&mut fields, source, map.next_value::<String>()?)
                            .map_err(|_| serde::de::Error::custom("duplicate config root"))?;
                    } else {
                        let _: IgnoredAny = map.next_value()?;
                    }
                }
                Ok(fields)
            }
        }
        deserializer.deserialize_map(Fields)
    }
}
pub fn root_fields(bytes: &[u8], format: &str) -> Result<RootFields> {
    let invalid = || Error::attention("invalid_chezmoi_config", "配置路径字段无法静态解析");
    match format {
        "json" => serde_json::from_slice(bytes).map_err(|_| invalid()),
        "toml" => {
            let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
            let doc = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|_| invalid())?;
            let mut fields = RootFields::default();
            for (key, value) in doc.iter() {
                if let Some(source) = root_key(key).map_err(|_| invalid())? {
                    set_field(
                        &mut fields,
                        source,
                        value.as_str().ok_or_else(invalid)?.into(),
                    )
                    .map_err(|_| invalid())?;
                }
            }
            Ok(fields)
        }
        _ => Err(Error::attention(
            "unsupported_chezmoi_config",
            "配置格式未接入静态路径读取",
        )),
    }
}
pub fn path_value(value: &str, home: &Path) -> Result<PathBuf> {
    if value.is_empty() || value.contains(['\0', '$']) || value.contains("{{") {
        return Err(Error::attention(
            "unsupported_config_path",
            "配置路径不是支持的静态绝对路径",
        ));
    }
    let path = value.strip_prefix("~/").map_or_else(
        || PathBuf::from(value),
        |tail| home.join(tail.trim_start_matches('/')),
    );
    crate::policy::paths::lexical(&path)
}
pub fn xdg_home(value: Option<&OsStr>, home: &Path, fallback: &str) -> Result<PathBuf> {
    match value.filter(|value| !value.is_empty()) {
        Some(value) => {
            let value = value.to_str().ok_or_else(|| {
                Error::attention("invalid_config_environment", "目录环境无法表示")
            })?;
            crate::policy::paths::lexical(Path::new(value))
        }
        None => Ok(home.join(fallback)),
    }
}
pub fn xdg_dirs(value: Option<&OsStr>, defaults: &[&str]) -> Result<Vec<PathBuf>> {
    let raw = match value.filter(|value| !value.is_empty()) {
        Some(value) => Some(value.to_str().ok_or_else(|| {
            Error::attention("invalid_config_environment", "目录搜索列表无法表示")
        })?),
        None => None,
    };
    let values = raw.map_or_else(|| defaults.to_vec(), |value| value.split(':').collect());
    if values.len() > 64 || values.iter().any(|value| value.is_empty()) {
        return Err(Error::attention(
            "invalid_config_environment",
            "目录搜索列表为空或过长",
        ));
    }
    values
        .into_iter()
        .map(|value| crate::policy::paths::lexical(Path::new(value)))
        .collect()
}

pub fn directive(bytes: &[u8]) -> Result<PathBuf> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| Error::new("invalid_chezmoiroot", "源根指令编码不兼容"))?
        .trim();
    if value.is_empty()
        || value.contains(['\0', '$'])
        || value.contains("{{")
        || Path::new(value)
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(Error::attention(
            "invalid_chezmoiroot",
            "源根指令不是安全相对目录",
        ));
    }
    Ok(value.into())
}

#[derive(Debug)]
pub struct Budget {
    pub remaining: usize,
    pub depth: usize,
}
impl Budget {
    pub fn new(entries: usize, depth: usize) -> Self {
        Self {
            remaining: entries,
            depth,
        }
    }
    pub fn reserve(&mut self, report: &mut Coverage) -> bool {
        if self.remaining == 0 {
            issue(report, CoverageState::Truncated, "config_scan_limit".into());
            false
        } else {
            self.remaining -= 1;
            true
        }
    }
    pub fn take(&mut self, report: &mut Coverage) -> bool {
        if !self.reserve(report) {
            return false;
        }
        report.count += 1;
        true
    }
}
pub fn issue(report: &mut Coverage, state: CoverageState, reason: String) {
    let rank = |state: &CoverageState| match state {
        CoverageState::Complete | CoverageState::Absent => 0,
        CoverageState::Unsupported => 1,
        CoverageState::Truncated => 2,
        CoverageState::Unavailable => 3,
        CoverageState::Invalid => 4,
    };
    if rank(&state) > rank(&report.state) {
        report.state = state;
    }
    report.issues.push(reason);
}

#[cfg(test)]
#[path = "../../../tests/unit/chezmoi.rs"]
mod tests;
