use crate::{
    config::rules::config_tx,
    error::{Error, Result},
    policy::operation::parsers::exact_version,
};
use toml_edit::{Document, Item};

pub const TOOLS: [&str; 7] = [
    "mimo",
    "codex",
    "claude",
    "cursor",
    "kimi",
    "dsh",
    "codegraph",
];

pub fn upgrade_target(id: &str) -> Option<&str> {
    id.strip_prefix("ai:").filter(|name| TOOLS.contains(name))
}

pub fn binary(tool: &str) -> Result<&str> {
    if !TOOLS.contains(&tool) {
        return Err(Error::new("unsupported_ai_tool", tool));
    }
    Ok(if tool == "cursor" {
        "cursor-agent"
    } else {
        tool
    })
}

pub fn version(tool: &str, value: Option<&str>) -> Result<Option<String>> {
    binary(tool)?;
    let Some(value) = value else {
        return if matches!(tool, "dsh" | "codegraph") {
            Err(Error::new("exact_version_required", tool))
        } else {
            Ok(None)
        };
    };
    exact_version(value)?;
    if matches!(tool, "dsh" | "codegraph") {
        let (stable, pre) = value
            .split_once('-')
            .map_or((value, None), |(s, p)| (s, Some(p)));
        let parts: Vec<_> = stable.split('.').collect();
        if parts.len() != 3
            || parts
                .iter()
                .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
            || pre.is_some_and(|p| {
                p.is_empty()
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
            })
            || (tool == "codegraph" && pre.is_some())
        {
            return Err(Error::new("exact_version_required", tool));
        }
    }
    Ok(Some(value.into()))
}

pub fn running_version(text: &str) -> Result<String> {
    let versions: Vec<_> = text
        .split_whitespace()
        .filter_map(|word| {
            let word = word.strip_prefix('v').unwrap_or(word);
            exact_version(word)
                .ok()
                .filter(|v| v.contains('.') && v.bytes().any(|b| b.is_ascii_digit()))
        })
        .collect();
    match versions.as_slice() {
        [version] => Ok(version.clone()),
        _ => Err(Error::attention(
            "ai_version_unknown",
            "无法从版本探测中确定唯一版本",
        )),
    }
}

pub fn pin(text: &str, tool: &str) -> Result<String> {
    let spec = config_tx::ai_mise_spec(tool)?;
    let doc = Document::parse(text)
        .map_err(|_| Error::attention("invalid_mise_toml", "mise 配置不是合法 TOML"))?;
    let item = doc
        .get("tools")
        .and_then(Item::as_table_like)
        .and_then(|t| t.get(spec));
    let pin = item
        .and_then(|i| {
            i.as_str().or_else(|| {
                let t = i.as_inline_table()?;
                (t.len() == 1)
                    .then(|| t.get("version").and_then(toml_edit::Value::as_str))
                    .flatten()
            })
        })
        .ok_or_else(|| {
            Error::attention("unsupported_mise_pin_shape", "AI pin 必须是单一精确版本")
        })?;
    exact_version(pin)
}

pub fn rewrite(text: &str, tool: &str, target: &str) -> Result<String> {
    exact_version(target)?;
    let spec = config_tx::ai_mise_spec(tool)?;
    let doc = Document::parse(text)
        .map_err(|_| Error::attention("invalid_mise_toml", "mise 配置不是合法 TOML"))?;
    pin(text, tool)?;
    config_tx::rewrite_tool_pin_spanned(text, &doc, spec, target)
}

pub fn require_sync(live: &str, source: &str, force_live: bool) -> Result<()> {
    if !force_live
        && !matches!(
            config_tx::evaluate_mise_reconcile(live, source, None)?,
            config_tx::MiseReconcilePlan::InSync { .. }
        )
    {
        return Err(Error::attention(
            "mise_config_drift",
            "本机的 mise 设置和保存的来源不同。运行 mo config reconcile 查看差异。",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/ai_policy.rs"]
mod tests;
