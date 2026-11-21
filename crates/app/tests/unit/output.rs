#![cfg(test)]
use super::*;
use serde_json::json;
#[test]
fn human_list_and_operation_keep_ids_and_results_visible() {
    let text =
        human(&json!([{"id":"brew:demo","name":"demo","kind":"cli","provider":{"type":"brew"}}]));
    assert!(text.contains("brew:demo\tcli\tbrew\tdemo"));
    assert_eq!(
        human(&json!({"id":"OP-1","phase":"unknown","message":"需要恢复"})),
        "操作：OP-1\n结果：unknown\n需要恢复\n"
    );
    assert!(human(&json!({"mode":"dry-run","command":["demo"]})).contains("未执行写后端"));
}

#[test]
fn human_fallbacks_and_legacy_output_keep_information() {
    assert!(human(&json!([{"id":"demo","name":"demo"}])).contains("demo\t-\t-\tdemo"));
    assert_eq!(human(&json!([])), "ID\t类型\t维护渠道\t名称\n");
    assert!(human(&json!({"journal":{"id":"OP-1","phase":"succeeded","message":"done"},"output":"original backend output"})).ends_with("original backend output"));
    assert!(
        human(&json!({"journal":{"id":"OP-1","phase":"succeeded","message":"done"}}))
            .contains("done")
    );
    assert_eq!(
        human(&json!({"data":{"text":"legacy text"}})),
        "legacy text"
    );
    assert_eq!(human(&json!(true)), "true\n");
    assert!(human(&json!({"data":null})).contains("null"));
}
