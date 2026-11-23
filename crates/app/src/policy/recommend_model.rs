use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MODEL: &str = "deepseek-flash";
pub const ENDPOINT: &str = "https://api.deepseek.com/chat/completions";
pub const KEYCHAIN_SERVICE: &str = "org.macops.recommend.deepseek";
pub const RESPONSE_LIMIT: usize = 128 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Note {
    pub candidate: usize,
    pub summary: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Notes {
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub notes: Vec<Note>,
}

pub fn account(value: &str) -> Result<&str> {
    if value.is_empty()
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(Error::new(
            "recommend_model_account_invalid",
            "钥匙串账号名称无效",
        ));
    }
    Ok(value)
}

pub fn api_token(raw: &[u8]) -> Result<&str> {
    let token = raw.strip_suffix(b"\n").unwrap_or(raw);
    let token = std::str::from_utf8(token)
        .map_err(|_| Error::attention("recommend_model_key_invalid", "钥匙串项目格式无效"))?;
    if !(12..=512).contains(&token.len())
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(Error::attention(
            "recommend_model_key_invalid",
            "钥匙串项目格式无效",
        ));
    }
    Ok(token)
}

pub fn request(findings: &super::recommend::Analysis) -> String {
    request_for(findings, MODEL, true)
}

pub(crate) fn request_for(
    findings: &super::recommend::Analysis,
    model: &str,
    deepseek: bool,
) -> String {
    let candidates = findings.findings.iter().enumerate().map(|(index, finding)| {
        json!({"candidate":index,"facts":finding.candidate,"evidence":finding.evidence_urls})
    }).collect::<Vec<_>>();
    let facts = Value::Array(candidates).to_string();
    let mut body = json!({
        "model":model,
        "stream":false,
        "max_tokens":500,
        "response_format":{"type":"json_object"},
        "messages":[
            {"role":"system","content":"用中文解释给定的公开安装候选，只引用已有事实。返回 JSON 对象 { \"notes\": [{ \"candidate\": 0, \"summary\": \"简短事实说明\" }] }。编号来自输入。无需命令或链接。"},
            {"role":"user","content":facts}
        ]
    });
    if deepseek {
        body["thinking"] = json!({"type":"disabled"});
    }
    body.to_string()
}

fn invalid_response() -> Error {
    Error::attention(
        "recommend_model_no_verified_result",
        "模型结果无法按当前证据核验",
    )
}

pub fn response(bytes: &[u8], candidates: usize) -> Result<Notes> {
    if bytes.is_empty() || bytes.len() > RESPONSE_LIMIT {
        return Err(invalid_response());
    }
    let outer = crate::policy::json::parse(bytes).map_err(|_| invalid_response())?;
    let Some(choices) = outer.get("choices").and_then(Value::as_array) else {
        return Err(invalid_response());
    };
    let [choice] = choices.as_slice() else {
        return Err(invalid_response());
    };
    let message = &choice["message"];
    if choice["finish_reason"] != "stop"
        || message["role"] != "assistant"
        || !message["tool_calls"].is_null()
    {
        return Err(invalid_response());
    }
    let content = message["content"].as_str().ok_or_else(invalid_response)?;
    let content = crate::policy::json::parse(content.as_bytes()).map_err(|_| invalid_response())?;
    let crate::policy::json::Object(notes) =
        serde_json::from_value::<crate::policy::json::Object<Notes>>(content)
            .map_err(|_| invalid_response())?;
    let mut seen = BTreeSet::new();
    if notes.notes.is_empty() || notes.notes.len() > candidates {
        return Err(invalid_response());
    }
    for note in &notes.notes {
        if note.candidate >= candidates
            || !seen.insert(note.candidate)
            || note.summary.len() > 512
            || note.summary.trim().is_empty()
            || note.summary.chars().any(|c| {
                c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
        {
            return Err(invalid_response());
        }
    }
    Ok(notes)
}

#[cfg(test)]
#[path = "../../tests/unit/recommend_model_policy.rs"]
mod tests;
