use crate::{
    error::{Error, Result},
    policy::invocation::{self, Declaration, OverlayRecord},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Intent {
    Unmanaged,
    ExplicitManual,
    Mirror { upstream_manual: bool },
}
impl Intent {
    pub fn manual(&self) -> bool {
        matches!(
            self,
            Self::ExplicitManual
                | Self::Mirror {
                    upstream_manual: true
                }
        )
    }
}
pub struct Applied {
    pub declaration: Option<Declaration>,
    pub book: OverlayRecord,
}
pub fn sync_required() -> Error {
    Error::attention(
        "source_policy_sync_required",
        "调用策略声明与已生效账本不一致；请先执行 skills policy sync",
    )
}
pub fn drift() -> Error {
    Error::attention(
        "source_policy_drift",
        "当前所选技能未精确保持已生效的调用策略；先处理漂移",
    )
}
pub fn applied(home: &Path, rules: Option<&[u8]>, overlays: Option<&[u8]>) -> Result<Applied> {
    let declaration = rules.map(invocation::declaration).transpose()?;
    let book = overlays
        .map(|raw| invocation::record(home, raw))
        .transpose()?
        .unwrap_or(OverlayRecord {
            schema_version: 1,
            policy_hash: None,
            files: BTreeMap::new(),
            profile_policy_hash: None,
            profiles: BTreeMap::new(),
        });
    let expected = rules.map(invocation::hash);
    if book.policy_hash != expected || (rules.is_none() && !book.files.is_empty()) {
        return Err(sync_required());
    }
    Ok(Applied { declaration, book })
}
pub fn intent(
    declaration: Option<&Declaration>,
    name: &str,
    upstream_manual: Option<bool>,
) -> Result<Intent> {
    let Some(declaration) = declaration else {
        return Ok(Intent::Unmanaged);
    };
    if declaration
        .standalone_manual_only
        .iter()
        .any(|entry| entry == name)
    {
        return Ok(Intent::ExplicitManual);
    }
    if declaration.mirror_standalone_upstream_manual
        && !name.to_ascii_lowercase().starts_with("devnorm")
    {
        return upstream_manual
            .map(|upstream_manual| Intent::Mirror { upstream_manual })
            .ok_or_else(sync_required);
    }
    Ok(Intent::Unmanaged)
}

#[cfg(test)]
#[path = "../../tests/unit/source_invocation_policy.rs"]
mod tests;
