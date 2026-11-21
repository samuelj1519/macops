#![cfg(test)]
use super::*;
use crate::test_support::TestResult;

#[test]
fn metadata_json_rejects_duplicate_keys_at_every_depth_without_losing_valid_values() -> TestResult {
    let input = br#"{"name":"skill","nested":[null,true,-2,5,1.25,{"key":"value"}]}"#;
    assert_eq!(parse(input)?, serde_json::from_slice::<Value>(input)?);
    for duplicate in [
        br#"{"a":1,"a":2}"#.as_slice(),
        br#"{"nested":[{"a":1,"a":2}]}"#,
    ] {
        assert!(
            parse(duplicate).is_err(),
            "duplicate metadata must not silently overwrite an earlier value"
        );
    }
    Ok(())
}

#[test]
fn json_rejects_non_finite_scalars_decoded_duplicate_names_and_reports_only_schema_expectations() {
    let error: serde::de::value::Error =
        serde::de::Error::invalid_type(serde::de::Unexpected::Bytes(b"x"), &JsonVisitor);
    assert!(error.to_string().contains("unique object keys"));
    assert!(
        JsonVisitor
            .visit_f64::<serde::de::value::Error>(f64::NAN)
            .is_err()
    );
    assert!(
        JsonVisitor
            .visit_f64::<serde::de::value::Error>(f64::INFINITY)
            .is_err()
    );
    assert!(parse(br#"{"name":1,"\u006eame":2}"#).is_err());
    assert!(parse(b"{bad").is_err());
    assert!(parse(b"1e999").is_err());
}
