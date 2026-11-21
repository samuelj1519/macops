use serde_json::Value;
use std::fmt::Write;

pub fn human(data: &Value) -> String {
    if let Some(rows) = data.as_array()
        && rows
            .iter()
            .all(|r| r.get("id").is_some() && r.get("name").is_some())
    {
        let mut text = String::from("ID\t类型\t维护渠道\t名称\n");
        for row in rows {
            let _ = writeln!(
                text,
                "{}\t{}\t{}\t{}",
                field(row, "id"),
                field(row, "kind"),
                row.get("provider").map_or("-", |p| field(p, "type")),
                field(row, "name")
            );
        }
        return text;
    }
    if let Some(journal) = data.get("journal") {
        return human(journal) + data.get("output").and_then(Value::as_str).unwrap_or("");
    }
    if data.get("phase").is_some() {
        return format!(
            "操作：{}\n结果：{}\n{}\n",
            field(data, "id"),
            field(data, "phase"),
            field(data, "message")
        );
    }
    if data.get("mode").and_then(Value::as_str) == Some("dry-run") {
        return format!(
            "预演：未执行写后端，未创建持久状态。\n{}\n",
            serde_json::to_string_pretty(data).unwrap_or_default()
        );
    }
    if let Some(text) = data
        .get("data")
        .and_then(|d| d.get("text"))
        .and_then(Value::as_str)
    {
        return text.to_owned();
    }
    serde_json::to_string_pretty(data).unwrap_or_default() + "\n"
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("-")
}

#[cfg(test)]
#[path = "../tests/unit/output.rs"]
mod tests;
