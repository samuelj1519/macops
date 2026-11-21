use super::*;
use crate::test_support::TestResult;

fn wrap(notes: &Value) -> Value {
    json!({"choices":[{
        "finish_reason":"stop",
        "message":{"role":"assistant","content":notes.to_string()}
    }]})
}
fn note(candidate: usize, summary: &str) -> Value {
    json!({"candidate":candidate,"summary":summary})
}

#[test]
fn model_policy_requires_one_completed_assistant_and_bound_candidates() -> TestResult {
    let good = wrap(&json!({"notes":[note(1, "选择范围与证据一致")]}));
    assert_eq!(
        response(&serde_json::to_vec(&good)?, 2)?.notes[0].candidate,
        1
    );
    for outer in [json!({}), json!({"choices":[]}), json!({"choices":[1,2]})] {
        assert!(response(&serde_json::to_vec(&outer)?, 2).is_err());
    }
    for change in [
        "truncated",
        "role",
        "tool",
        "missing_text",
        "not_json",
        "bad_object",
    ] {
        let mut output = good.clone();
        let row = &mut output["choices"][0];
        match change {
            "truncated" => row["finish_reason"] = json!("length"),
            "role" => row["message"]["role"] = json!("user"),
            "tool" => row["message"]["tool_calls"] = json!([{}]),
            "missing_text" => row["message"]["content"] = Value::Null,
            "not_json" => row["message"]["content"] = json!("not JSON"),
            "bad_object" => row["message"]["content"] = json!("[[1]]"),
            _ => return Err("unexpected case".into()),
        }
        assert!(
            response(&serde_json::to_vec(&output)?, 2).is_err(),
            "{change}"
        );
    }
    for notes in [
        json!({"notes":[]}),
        json!({"notes":[[0,"invalid positional note"]]}),
        json!({"notes":[note(0,"ok"),note(1,"ok"),note(2,"too many")]}),
        json!({"notes":[note(3,"unknown")]}),
        json!({"notes":[note(1,"one"),note(1,"duplicate")]}),
        json!({"notes":[note(1,&"x".repeat(513))]}),
        json!({"notes":[note(1,"")]}),
        json!({"notes":[note(1,"A\nB")]}),
        json!({"notes":[note(1,"A\u{202e}B")]}),
        json!({"notes":[note(1,"A\u{2066}B")]}),
        json!({"notes":[{"candidate":1,"summary":"fine","execute":"unknown"}]}),
    ] {
        assert!(response(&serde_json::to_vec(&wrap(&notes))?, 2).is_err());
    }
    assert!(response(b"", 2).is_err());
    assert!(response(b"not a document", 2).is_err());
    assert!(response(&vec![b' '; RESPONSE_LIMIT + 1], 2).is_err());
    Ok(())
}

#[test]
fn model_policy_limits_model_input_and_validates_only_token_shapes() -> TestResult {
    for name in ["", &"x".repeat(65), "-bad", "invalid name"] {
        assert!(account(name).is_err());
    }
    assert_eq!(account("profile_1")?, "profile_1");
    for value in [b"other" as &[u8], b"separated token Value", b"\xff"] {
        assert!(api_token(value).is_err());
    }
    assert!(api_token(&vec![b'x'; 513]).is_err());
    assert_eq!(
        api_token(b"synthetic-profile-key\n")?,
        "synthetic-profile-key"
    );
    assert_eq!(
        api_token(b"synthetic-profile-key")?,
        "synthetic-profile-key"
    );
    let docs = crate::policy::recommend::public_analysis("node")?.ok_or("snapshot")?;
    let body: Value = serde_json::from_str(&request(docs.facts()))?;
    assert_eq!(body["model"], MODEL);
    assert_eq!(body["stream"], false);
    assert!(body.get("tools").is_none());
    assert!(
        body["messages"][1]["content"]
            .as_str()
            .ok_or("candidate")?
            .contains("mise.jdx.dev")
    );
    Ok(())
}
