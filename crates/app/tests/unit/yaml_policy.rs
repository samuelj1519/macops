#![cfg(test)]
use super::*;
use crate::test_support::TestResult;

#[test]
fn yaml_metadata_preserves_scalars_and_rejects_ambiguous_or_excessive_structure() -> TestResult {
    let input = b"name: example\nvalues: [null, true, -1, 5, 1.25, text]\n";
    assert_eq!(parse(input)?, serde_saphyr::from_slice::<Value>(input)?);
    for invalid in [
        "a: 1\na: 2\n",
        "a: {b: 1, b: 2}\n",
        "a: &a {x: 1}\nb: {<<: *a}\n",
        "a: !custom value\n",
        "[a,\n",
        "[1]\n---\n[2]\n",
        "x: .nan\n",
        "x: .inf\n",
        "{1: value}\n",
    ] {
        assert!(parse(invalid.as_bytes()).is_err());
    }
    assert_eq!(parse(b"")?, Value::Null);
    assert_eq!(parse(b" \n")?, Value::Null);
    assert!(parse(&vec![b' '; BYTE_LIMIT + 1]).is_err());
    assert!(
        parse(
            format!(
                "{}0{}",
                "[".repeat(DEPTH_LIMIT + 2),
                "]".repeat(DEPTH_LIMIT + 2)
            )
            .as_bytes()
        )
        .is_err()
    );
    assert!(parse(format!("[{}]", "null,".repeat(NODE_LIMIT)).as_bytes()).is_err());
    Ok(())
}

#[test]
fn yaml_alias_expansion_and_invalid_keys_cannot_escape_the_structure_budget() {
    let input = format!(
        "a: &source [{}]\nb: [{}]\n",
        "x,".repeat(1000),
        "*source,".repeat(30)
    );
    assert!(parse(input.as_bytes()).is_err());
    assert!(parse(b"? [complex,key]\n: value\n").is_err());
    assert!(parse(b"unknown: !tag value\n").is_err());
    let nodes = Cell::new(0);
    let seed = Seed {
        nodes: &nodes,
        depth: 0,
    };
    let error: serde::de::value::Error =
        serde::de::Error::invalid_type(serde::de::Unexpected::Bytes(b"x"), &YamlVisitor(seed));
    assert!(error.to_string().contains("bounded metadata YAML"));
}

#[test]
fn tagged_yaml_keys_are_rejected_before_their_values_are_read() {
    assert!(parse(b"field: valid\n!unknown key: value\n").is_err());
}

#[test]
fn yaml_visitor_handles_an_absent_scalar_and_rejects_nonfinite_callbacks() {
    let nodes = Cell::new(0);
    let absent = YamlVisitor(Seed {
        nodes: &nodes,
        depth: 0,
    })
    .visit_none::<serde_saphyr::Error>();
    assert!(matches!(absent, Ok(Value::Null)));
    assert!(
        YamlVisitor(Seed {
            nodes: &nodes,
            depth: 0
        })
        .visit_f64::<serde_saphyr::Error>(f64::NAN)
        .is_err()
    );
    assert!(
        YamlVisitor(Seed {
            nodes: &nodes,
            depth: 0
        })
        .visit_f64::<serde_saphyr::Error>(f64::NEG_INFINITY)
        .is_err()
    );
}
