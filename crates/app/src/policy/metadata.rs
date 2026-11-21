use crate::{
    domain::ProcessOutput,
    error::{Error, Result},
};
use clap::ValueEnum;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Profile {
    Glm,
    Kimi,
    Mi,
    Zen,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Self::Glm => "glm",
            Self::Kimi => "kimi",
            Self::Mi => "mi",
            Self::Zen => "zen",
        }
    }
}

pub const PROFILES: [Profile; 4] = [Profile::Glm, Profile::Kimi, Profile::Mi, Profile::Zen];
pub const KEYCHAIN_SERVICE: &str = "local.claude-profile";

pub fn valid_alias(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}

pub fn alias(value: &str) -> Result<String> {
    if valid_alias(value) {
        Ok(value.into())
    } else {
        Err(Error::new(
            "invalid_alias",
            "alias 仅允许字母、数字、点、下划线和短横线",
        ))
    }
}

pub fn active_account(value: &Value, helper: &Path, profile: Profile) -> Option<String> {
    let reference = value.get("apiKeyHelper")?.as_str()?;
    let account = reference.strip_prefix(&format!("{} ", helper.display()))?;
    let name = account.strip_prefix(&format!("claude-{}@", profile.name()))?;
    valid_alias(name).then(|| account.to_owned())
}

pub fn keychain_status(code: Option<i32>, uncertain: bool) -> &'static str {
    if code == Some(0) && !uncertain {
        "present"
    } else {
        "unknown"
    }
}

pub fn limited_lines(text: &str) -> Value {
    let lines: Vec<_> = text.lines().filter(|line| !line.is_empty()).collect();
    limited_values(&lines)
}
pub fn limited_values<T: AsRef<str>>(values: &[T]) -> Value {
    json!({"count":values.len(),"shown":values.iter().take(80).map(AsRef::as_ref).collect::<Vec<_>>(),"truncated":values.len()>80})
}

pub fn query_text(output: &ProcessOutput, allow_no_matches: bool) -> Result<&str> {
    if output.uncertain || !(output.code == Some(0) || allow_no_matches && output.code == Some(1)) {
        return Err(Error::attention(
            "configuration_unobserved",
            format!("配置查询未完成或失败，原生 exit {:?}", output.code),
        ));
    }
    Ok(&output.stdout)
}

#[cfg(test)]
#[path = "../../tests/unit/metadata.rs"]
mod tests;
