#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use serde_json::json;
#[test]
fn owned_json_fields_change_without_reencoding_model_credential_or_other_raw_values() -> TestResult
{
    let before = br#"{
  "model" : "custom-\u006dodel", "env": { "TOKEN" : "PRIVATE\\VALUE" },
  "enabledPlugins": { "target@vendor" : true, "other@vendor" : true }
}
"#;
    let after = apply(
        before,
        &[Edit {
            path: vec!["enabledPlugins".into(), "target@vendor".into()],
            value: Some(json!(false)),
        }],
    )?;
    let value: Value = crate::policy::json::parse(&after)?;
    assert_eq!(value["enabledPlugins"]["target@vendor"], false);
    assert!(
        String::from_utf8(after.clone())?
            .contains(r#""model" : "custom-\u006dodel", "env": { "TOKEN" : "PRIVATE\\VALUE" }"#)
    );
    assert!(String::from_utf8(after)?.contains(r#""other@vendor" : true"#));
    Ok(())
}

fn edit(path: &[&str], value: Option<Value>) -> Edit {
    Edit {
        path: path.iter().map(|s| (*s).into()).collect(),
        value,
    }
}
#[test]
fn adding_removing_and_noops_keep_whitespace_and_unmodified_subtrees_exact() -> TestResult {
    let original=b" \n{\r\n \"model\" : \"x\\u0061\", \"remove\" : true, \"last\":{ \"key\": [1, 2] }\r\n}\n ";
    assert_eq!(apply(original, &[])?, original);
    let after = apply(
        original,
        &[
            edit(&["remove"], None),
            edit(&["plugins", "new@vendor"], Some(json!(false))),
        ],
    )?;
    let text = String::from_utf8(after.clone())?;
    assert!(text.starts_with(" \n{"));
    assert!(text.ends_with("}\n "));
    assert!(text.contains(r#""model" : "x\u0061""#));
    assert!(text.contains(r#""last":{ "key": [1, 2] }"#));
    let parsed = crate::policy::json::parse(&after)?;
    assert!(parsed.get("remove").is_none());
    assert_eq!(parsed["plugins"]["new@vendor"], false);
    assert_eq!(
        crate::policy::json::parse(&apply(
            br#"{"a":1,"b":2,"c":3}"#,
            &[edit(&["a"], None), edit(&["b"], None), edit(&["c"], None)]
        )?)?,
        json!({})
    );
    assert_eq!(
        crate::policy::json::parse(&apply(b"{ }", &[edit(&["missing", "value"], None)])?)?,
        json!({})
    );
    Ok(())
}
#[test]
fn array_edits_preserve_existing_string_escapes_and_duplicate_occurrence_order() -> TestResult {
    let raw = br#"{"deny":["Bash(\u0063at)", "keep", "Bash(cat)"]}"#;
    let out = apply(
        raw,
        &[edit(
            &["deny"],
            Some(json!(["Bash(cat)", "Bash(cat)", "new"])),
        )],
    )?;
    assert!(String::from_utf8(out)?.contains(r#"["Bash(\u0063at)", "Bash(cat)", "new"]"#));
    assert_eq!(
        crate::policy::json::parse(&apply(b"{\"a\":[]}", &[edit(&["a"], Some(json!([1])))])?)?,
        json!({"a":[1]})
    );
    assert_eq!(
        crate::policy::json::parse(&apply(b"{\"a\":[1]}", &[edit(&["a"], Some(json!([])))])?)?,
        json!({"a":[]})
    );
    Ok(())
}
#[test]
fn malformed_ambiguous_or_over_budget_edits_never_return_a_rewritten_file() {
    for raw in [b"[]".as_slice(), b"{", br#"{"a":1,"a":2}"#] {
        assert!(apply(raw, &[]).is_err());
    }
    assert!(apply(br#"{"a":null}"#, &[edit(&["a", "b"], Some(json!(1)))]).is_err());
    assert!(apply(b"{}", &[edit(&[], Some(json!(1)))]).is_err());
    assert!(
        apply(
            b"{}",
            &[
                edit(&["a"], Some(json!(1))),
                edit(&["a", "b"], Some(json!(2)))
            ]
        )
        .is_err()
    );
    let deep = format!("{{\"a\":{}0{}}}", "[".repeat(34), "]".repeat(34));
    assert!(apply(deep.as_bytes(), &[]).is_err());
    assert!(apply(&vec![b' '; 1024 * 1024 + 1], &[]).is_err());
    assert!(apply(b"{}", &vec![edit(&["a"], Some(json!(1))); 1001]).is_err());
}

#[test]
fn json_output_budget_limits_raw_spacing_expansion_and_new_values_before_returning_bytes()
-> TestResult {
    let raw = format!("{{\"a\":1,{}\"b\":2}}", " ".repeat(700_000));
    assert!(
        apply(
            raw.as_bytes(),
            &[edit(&["c"], Some(json!(3))), edit(&["d"], Some(json!(4)))]
        )
        .is_err()
    );
    assert!(
        apply(
            b"{}",
            &[edit(&["large"], Some(json!("x".repeat(1024 * 1024))))]
        )
        .is_err()
    );
    assert!(
        apply(
            b"{}",
            &[Edit {
                path: vec!["a".into(); 33],
                value: Some(json!(1))
            }]
        )
        .is_err()
    );
    assert!(
        apply(
            b"{}",
            &[edit(&["a"], Some(json!(1))), edit(&["a"], Some(json!(2)))]
        )
        .is_err()
    );
    for (raw, after) in [
        (br#"{"a":1}"#.as_slice(), json!({"nested":[true,null]})),
        (br#"{"a":false}"#, json!([null, 1])),
        (br#"{"a":[1,2]}"#, json!([2, 1, 3])),
    ] {
        let out = apply(raw, &[edit(&["a"], Some(after.clone()))])?;
        assert_eq!(crate::policy::json::parse(&out)?["a"], after);
    }
    Ok(())
}
#[test]
fn structural_scanning_and_output_proofs_reject_truncation_and_mismatched_source_trees()
-> TestResult {
    for raw in [
        b"".as_slice(),
        b"]",
        b"\"",
        b"{\"",
        br#"{"a" 1}"#,
        br#"{"a":1 "b":2}"#,
        br#"{"a":"unterminated}"#,
        br#"{"\q":1}"#,
        b"[1 2]",
    ] {
        let mut scanner = Scanner {
            bytes: raw,
            at: 0,
            left: 20000,
        };
        assert!(
            scanner.value(0).is_err(),
            "accepted malformed structural input {raw:?}"
        );
    }
    let mut scanner = Scanner {
        bytes: b"0",
        at: 0,
        left: 20000,
    };
    assert!(scanner.value(33).is_err());
    let mut scanner = Scanner {
        bytes: b"0",
        at: 0,
        left: 0,
    };
    assert!(scanner.value(0).is_err());
    let mut scanner = Scanner {
        bytes: b"not-quoted",
        at: 0,
        left: 10,
    };
    assert!(scanner.string().is_err());
    let mut scanner = Scanner {
        bytes: b"[]",
        at: 0,
        left: 10,
    };
    assert!(scanner.object(0).is_err());
    let mut scanner = Scanner {
        bytes: b"{}",
        at: 0,
        left: 10,
    };
    assert!(scanner.array(0).is_err());
    assert!(set(&mut json!({}), &[], Some(&json!(1))).is_err());
    for (raw, before, after) in [
        (
            br#"{"nested":{"keep":0}}"#.as_slice(),
            json!({"nested":{}}),
            json!({"nested":{"keep":1}}),
        ),
        (
            br#"{"items":[1]}"#,
            json!({"items":[1,2]}),
            json!({"items":[2]}),
        ),
    ] {
        let mut scanner = Scanner {
            bytes: raw,
            at: 0,
            left: 20000,
        };
        let node = scanner.value(0)?;
        let mut output = Output {
            limit: PROFILE.bytes,
            bytes: vec![],
            invalid: false,
        };
        render(&mut output, &node, &before, &after, raw);
        assert!(output.finish().is_err());
    }
    assert!(verify_output(br#"{"x":1}"#, &json!({"x":2}), PROFILE).is_err());
    assert!(verify_output(b"{", &json!({}), PROFILE).is_err());
    verify_output(b" {} \n", &json!({}), PROFILE)?;
    Ok(())
}

#[test]
fn governance_document_budget_does_not_raise_the_profile_configuration_budget() -> TestResult {
    let raw = serde_json::to_vec(&json!({"legacy":"x".repeat(1024*1024),"future":{"bytes":"中"}}))?;
    assert!(apply(&raw, &[]).is_err());
    assert_eq!(
        parse_governance(&raw)?["legacy"]
            .as_str()
            .ok_or("string")?
            .len(),
        1024 * 1024
    );
    let output = apply_governance(
        &raw,
        &[edit(
            &["_macops_sources"],
            Some(json!({"schema_version":1,"roots":{}})),
        )],
    )?;
    assert_eq!(
        crate::policy::json::parse(&output)?["_macops_sources"]["schema_version"],
        1
    );
    assert!(parse_governance(&vec![b' '; 16 * 1024 * 1024 + 1]).is_err());
    let nodes = format!("{{\"items\":[{}]}}", vec!["0"; 500_001].join(","));
    assert!(parse_governance(nodes.as_bytes()).is_err());
    assert!(apply_governance(b"{", &[]).is_err());
    Ok(())
}
