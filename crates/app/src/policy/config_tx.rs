use crate::{
    domain::{
        Action, ConfigAnchor, ConfigAuthority, ConfigFileStamp, FileId, Observation, Plan,
        Provider, Receipt,
    },
    error::{Error, Result},
    policy::{config_local::valid_digest, parsers::exact_version},
    resource::CoverageState,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
use toml_edit::{Document, Item, Value as TomlValue};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigBackupRecord {
    pub schema_version: u32,
    pub operation_id: String,
    pub config_id: String,
    pub target: PathBuf,
    pub parent_canonical: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub parent_identity: FileId,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub before_stamp: Option<ConfigFileStamp>,
    pub expected_after_digest: String,
    pub expected_after_mode: u32,
    #[serde(default)]
    pub expected_absent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconcile: Option<ReconcileGuard>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigRootInputs {
    pub source: Option<PathBuf>,
    pub destination: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub(crate) env: BTreeMap<String, PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RootGuard {
    pub inputs: ConfigRootInputs,
    pub isolated: bool,
    pub digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReconcileGuard {
    pub live: ConfigAnchor,
    pub stamp: ConfigFileStamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<RootGuard>,
}

pub fn validate_backup_record(record: &ConfigBackupRecord) -> Result<()> {
    crate::policy::paths::lexical(&record.target)?;
    crate::policy::paths::lexical(&record.parent_canonical)?;
    if record.schema_version != 1
        || !crate::policy::state::valid_id(&record.operation_id)
        || record.config_id != crate::policy::config_local::config_id(&record.target)?
        || !valid_digest(&record.expected_after_digest)
        || record.expected_after_mode > 0o7777
        || record.before_stamp.as_ref().is_some_and(|s| {
            s.mode > 0o7777 || !valid_digest(&s.content_digest) || !valid_digest(&s.fingerprint)
        })
    {
        return Err(Error::new("invalid_config_backup", "配置备份元数据不合法"));
    }
    Ok(())
}

pub fn validate_static_apply_eligibility(
    coverage_state: &CoverageState,
    coverage_issues: &[String],
    receipt: Option<&Receipt>,
    current_target: Option<&ConfigFileStamp>,
) -> Result<Vec<String>> {
    if matches!(
        coverage_state,
        CoverageState::Unavailable | CoverageState::Invalid | CoverageState::Truncated
    ) {
        return Err(Error::attention(
            "config_authority_uncertain",
            "配置来源扫描不完整，拒绝写覆盖",
        ));
    }
    let controls: Vec<&String> = coverage_issues
        .iter()
        .filter(|issue| issue.starts_with("source_control_not_evaluated:"))
        .collect();
    if controls.is_empty() {
        return Ok(vec![]);
    }
    let adopted_matches = receipt.is_some_and(|rec| {
        matches!(
            &rec.observation,
            Observation::ConfigFile {
                authority: ConfigAuthority::ChezmoiCandidate,
                target: Some(base_tgt),
                ..
            } if current_target == Some(base_tgt)
        )
    });
    if !adopted_matches {
        return Err(Error::attention(
            "dynamic_chezmoi_control_present",
            "存在未求值的 chezmoi 控制文件；未采纳或已漂移的目标拒绝自动写应用",
        ));
    }
    let listed = controls
        .into_iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    Ok(vec![format!(
        "存在未求值控制文件（基于既有采纳基线放行）：{listed}"
    )])
}

pub fn validate_config_tx_plan(plan: &Plan) -> Result<()> {
    if plan.target.is_none() && plan.action != Action::Restore {
        return Err(Error::new("invalid_config_plan", "配置写计划缺少预期摘要"));
    }
    if plan.command.is_some()
        || plan.delegated
        || !plan.synchronous
        || plan
            .target
            .as_ref()
            .is_some_and(|digest| !valid_digest(digest))
    {
        return Err(Error::new("invalid_config_plan", "配置写计划结构无效"));
    }
    let valid_shape = matches!(
        (plan.action, &plan.package.provider, &plan.before),
        (
            Action::Apply,
            Provider::ConfigObserved {
                authority: ConfigAuthority::ChezmoiCandidate,
                source: Some(_),
                ..
            },
            Observation::ConfigFile {
                authority: ConfigAuthority::ChezmoiCandidate,
                source: Some(_),
                ..
            },
        ) | (
            Action::Reconcile,
            Provider::ConfigObserved { .. },
            Observation::ConfigFile {
                target: Some(_),
                ..
            },
        ) | (
            Action::Restore,
            Provider::ConfigObserved { .. },
            Observation::ConfigFile { .. }
        )
    );
    if valid_shape {
        Ok(())
    } else {
        Err(Error::new(
            "invalid_config_plan",
            "配置写动作与观察类型不匹配",
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreDecision {
    AlreadyAtBefore,
    ReadyToRestore,
    ExternalConflict,
}

pub fn restored_stamp_matches(
    before: Option<&ConfigFileStamp>,
    after: Option<&ConfigFileStamp>,
) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(b), Some(a)) => a.content_digest == b.content_digest && a.mode == b.mode,
        _ => false,
    }
}

pub fn decide_restore(
    backup: &ConfigBackupRecord,
    current: Option<&ConfigFileStamp>,
) -> RestoreDecision {
    if restored_stamp_matches(backup.before_stamp.as_ref(), current) {
        return RestoreDecision::AlreadyAtBefore;
    }
    if let Some(cur) = current
        && cur.content_digest == backup.expected_after_digest
        && cur.mode == backup.expected_after_mode
    {
        return RestoreDecision::ReadyToRestore;
    }
    RestoreDecision::ExternalConflict
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryDecision {
    PreparedUntouched,
    AlreadyCommitted,
    CommitVerifiedWrite,
    RollbackFailedWrite,
    UnknownExternalConflict,
}

pub fn decide_tx_recovery(
    is_prepared: bool,
    receipt_committed: bool,
    backup: &ConfigBackupRecord,
    current: Option<&ConfigFileStamp>,
    written_candidate_digest: Option<&str>,
) -> RecoveryDecision {
    if (backup.expected_absent && current.is_none())
        || current.is_some_and(|c| {
            c.content_digest == backup.expected_after_digest && c.mode == backup.expected_after_mode
        })
    {
        return if receipt_committed {
            RecoveryDecision::AlreadyCommitted
        } else {
            RecoveryDecision::CommitVerifiedWrite
        };
    }
    if current == backup.before_stamp.as_ref() {
        return RecoveryDecision::PreparedUntouched;
    }
    if !is_prepared
        && let (Some(cur), Some(cand)) = (current, written_candidate_digest)
        && cur.content_digest == cand
    {
        return RecoveryDecision::RollbackFailedWrite;
    }
    RecoveryDecision::UnknownExternalConflict
}

pub fn ai_mise_spec(tool: &str) -> Result<&'static str> {
    match tool {
        "codex" => Ok("aqua:openai/codex"),
        "claude" => Ok("aqua:anthropics/claude-code"),
        "cursor" => Ok("http:cursor-agent"),
        "kimi" => Ok("npm:@moonshot-ai/kimi-code"),
        "dsh" => Ok("npm:@deepseek-ai/dsh"),
        "codegraph" => Ok("npm:@colbymchenry/codegraph"),
        "mimo" => Err(Error::new(
            "vendor_tool_no_mise_pin",
            "mimo 由厂商安装器独立维护，不存在 mise 配置 pin",
        )),
        _ => Err(Error::new("unsupported_reconcile_tool", tool)),
    }
}

pub fn has_mise_env_override(key: &str) -> bool {
    key.len() > 5
        && key.as_bytes()[..5].eq_ignore_ascii_case(b"MISE_")
        && !key.eq_ignore_ascii_case("MISE_SHELL")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MiseDriftRow {
    pub key: String,
    pub live: String,
    pub declared: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MiseReconcilePlan {
    InSync {
        spec: Option<String>,
        pin: Option<String>,
    },
    Drift {
        reason: String,
        rows: Vec<MiseDriftRow>,
    },
    Rewrite {
        spec: String,
        from: String,
        to: String,
        rewritten: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ToolEntry {
    Pin(String),
    Unsupported(String),
}

fn canonical_value(val: &TomlValue) -> String {
    match val {
        TomlValue::String(s) => format!("{:?}", s.value()),
        TomlValue::Integer(i) => format!("integer:{}", i.value()),
        TomlValue::Float(f) => format!("float:{}", f.value().to_bits()),
        TomlValue::Boolean(b) => b.value().to_string(),
        TomlValue::Datetime(d) => format!("{:?}", d.value()),
        TomlValue::Array(arr) => format!(
            "[{}]",
            arr.iter()
                .map(canonical_value)
                .collect::<Vec<_>>()
                .join(",")
        ),
        TomlValue::InlineTable(tbl) => {
            let mut pairs: Vec<_> = tbl
                .iter()
                .map(|(k, v)| format!("{k:?}={}", canonical_value(v)))
                .collect();
            pairs.sort();
            format!("{{{}}}", pairs.join(","))
        }
    }
}

fn canonical_item(item: &Item) -> String {
    if let Some(val) = item.as_value() {
        return canonical_value(val);
    }
    if let Some(tbl) = item.as_table_like() {
        let mut pairs: Vec<_> = tbl
            .iter()
            .map(|(k, v)| format!("{k:?}={}", canonical_item(v)))
            .collect();
        pairs.sort();
        return format!("table{{{}}}", pairs.join(","));
    }
    format!("{item:?}")
}

fn collect_entries(
    doc: &Document<&str>,
) -> (BTreeMap<String, ToolEntry>, BTreeMap<String, String>) {
    let mut tools = BTreeMap::new();
    let mut others = BTreeMap::new();
    for (top_key, item) in doc.iter() {
        if top_key == "tools"
            && let Some(tbl) = item.as_table_like()
        {
            for (k, v) in tbl.iter() {
                tools.insert(k.to_owned(), classify_tool_item(v));
            }
            continue;
        }
        if top_key == "env"
            && let Some(tbl) = item.as_table_like()
        {
            for (k, v) in tbl.iter() {
                others.insert(format!("[env] {k}"), format!("env:{}", canonical_item(v)));
            }
            continue;
        }
        if let Some(tbl) = item.as_table_like() {
            for (k, v) in tbl.iter() {
                others.insert(format!("[{top_key}] {k}"), canonical_item(v));
            }
        } else {
            others.insert(top_key.to_owned(), canonical_item(item));
        }
    }
    (tools, others)
}

fn classify_tool_item(item: &Item) -> ToolEntry {
    if let Some(val) = item.as_value() {
        if let Some(s) = val.as_str() {
            return ToolEntry::Pin(s.to_owned());
        }
        if let Some(inline) = val.as_inline_table()
            && inline.len() == 1
            && let Some(ver) = inline.get("version").and_then(TomlValue::as_str)
        {
            return ToolEntry::Pin(ver.to_owned());
        }
    }
    ToolEntry::Unsupported(canonical_item(item))
}

fn redact_if_env(_key: &str, _raw: &str) -> String {
    "<redacted>".into()
}

fn diff_maps(
    live_tools: &BTreeMap<String, ToolEntry>,
    src_tools: &BTreeMap<String, ToolEntry>,
    live_others: &BTreeMap<String, String>,
    src_others: &BTreeMap<String, String>,
    exclude_tool: Option<&str>,
) -> Vec<MiseDriftRow> {
    let mut rows = Vec::new();
    let render_tool = |e: &ToolEntry| match e {
        ToolEntry::Pin(v) => v.clone(),
        ToolEntry::Unsupported(_) => "<unsupported>".into(),
    };
    for (k, lv) in live_tools {
        if exclude_tool == Some(k.as_str()) {
            continue;
        }
        match src_tools.get(k) {
            Some(sv) if sv == lv => (),
            Some(sv) => rows.push(MiseDriftRow {
                key: k.clone(),
                live: render_tool(lv),
                declared: render_tool(sv),
            }),
            None => rows.push(MiseDriftRow {
                key: k.clone(),
                live: render_tool(lv),
                declared: "(缺失)".into(),
            }),
        }
    }
    for (k, sv) in src_tools {
        if exclude_tool == Some(k.as_str()) || live_tools.contains_key(k) {
            continue;
        }
        rows.push(MiseDriftRow {
            key: k.clone(),
            live: "(缺失)".into(),
            declared: render_tool(sv),
        });
    }
    for (k, lv) in live_others {
        match src_others.get(k) {
            Some(sv) if sv == lv => (),
            Some(sv) => rows.push(MiseDriftRow {
                key: k.clone(),
                live: redact_if_env(k, lv),
                declared: redact_if_env(k, sv),
            }),
            None => rows.push(MiseDriftRow {
                key: k.clone(),
                live: redact_if_env(k, lv),
                declared: "(缺失)".into(),
            }),
        }
    }
    for (k, sv) in src_others {
        if !live_others.contains_key(k) {
            rows.push(MiseDriftRow {
                key: k.clone(),
                live: "(缺失)".into(),
                declared: redact_if_env(k, sv),
            });
        }
    }
    rows
}

pub fn evaluate_mise_reconcile(
    live_text: &str,
    source_text: &str,
    tool: Option<&str>,
) -> Result<MiseReconcilePlan> {
    let live_doc = Document::parse(live_text)
        .map_err(|_| Error::attention("invalid_mise_toml", "本机 mise 配置文件不是合法 TOML"))?;
    let src_doc = Document::parse(source_text).map_err(|_| {
        Error::attention(
            "invalid_mise_toml",
            "chezmoi 声明的 mise 源文件不是合法 TOML",
        )
    })?;
    let (live_tools, live_others) = collect_entries(&live_doc);
    let (src_tools, src_others) = collect_entries(&src_doc);

    let Some(tool_name) = tool else {
        let rows = diff_maps(&live_tools, &src_tools, &live_others, &src_others, None);
        if rows.is_empty() && live_text == source_text {
            return Ok(MiseReconcilePlan::InSync {
                spec: None,
                pin: None,
            });
        }
        let reason = if rows.is_empty() {
            "formatting_or_comment_drift"
        } else {
            "mise_config_drift"
        };
        return Ok(MiseReconcilePlan::Drift {
            reason: reason.into(),
            rows,
        });
    };

    let spec = ai_mise_spec(tool_name)?;
    let other_rows = diff_maps(
        &live_tools,
        &src_tools,
        &live_others,
        &src_others,
        Some(spec),
    );
    if !other_rows.is_empty() {
        return Ok(MiseReconcilePlan::Drift {
            reason: "unrelated_keys_drifted".into(),
            rows: other_rows,
        });
    }
    let Some(src_entry) = src_tools.get(spec) else {
        return Ok(MiseReconcilePlan::Drift {
            reason: "missing_in_source".into(),
            rows: vec![],
        });
    };
    let Some(live_entry) = live_tools.get(spec) else {
        return Ok(MiseReconcilePlan::Drift {
            reason: "missing_in_live".into(),
            rows: vec![],
        });
    };
    let (ToolEntry::Pin(src_pin), ToolEntry::Pin(live_pin)) = (src_entry, live_entry) else {
        return Err(Error::attention(
            "unsupported_mise_pin_shape",
            "目标工具的 [tools] 条目必须是单一版本字符串或仅含 version 的内联表",
        ));
    };
    exact_version(src_pin)?;
    exact_version(live_pin)?;
    if src_pin == live_pin {
        return if live_text == source_text {
            Ok(MiseReconcilePlan::InSync {
                spec: Some(spec.into()),
                pin: Some(live_pin.clone()),
            })
        } else {
            Ok(MiseReconcilePlan::Drift {
                reason: "formatting_or_comment_drift".into(),
                rows: vec![],
            })
        };
    }
    rewrite_tool_pin_spanned(source_text, &src_doc, spec, live_pin).map(|rewritten| {
        if rewritten == live_text {
            MiseReconcilePlan::Rewrite {
                spec: spec.into(),
                from: src_pin.clone(),
                to: live_pin.clone(),
                rewritten,
            }
        } else {
            MiseReconcilePlan::Drift {
                reason: "formatting_or_comment_drift".into(),
                rows: vec![],
            }
        }
    })
}

pub fn rewrite_tool_pin_spanned(
    source_text: &str,
    doc: &Document<&str>,
    spec: &str,
    new_pin: &str,
) -> Result<String> {
    let val = doc
        .get("tools")
        .and_then(Item::as_table_like)
        .and_then(|t| t.get(spec))
        .and_then(Item::as_value)
        .ok_or_else(|| Error::new("invalid_mise_toml", "缺少目标键标量值"))?;
    let span = match val {
        TomlValue::String(s) => s.span(),
        TomlValue::InlineTable(inline) => inline.get("version").and_then(TomlValue::span),
        _ => None,
    }
    .ok_or_else(|| Error::new("unsupported_mise_pin_shape", "目标键缺少可定位版本字面量"))?;
    let mut out = String::with_capacity(source_text.len() + new_pin.len());
    out.push_str(&source_text[..span.start]);
    out.push('"');
    out.push_str(new_pin);
    out.push('"');
    out.push_str(&source_text[span.end..]);
    Ok(out)
}

#[cfg(test)]
#[path = "../../tests/unit/config_tx_policy.rs"]
mod tests;
