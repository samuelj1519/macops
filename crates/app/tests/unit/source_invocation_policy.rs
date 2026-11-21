#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use serde_json::json;
#[test]
fn source_intent_requires_applied_hash_and_distinguishes_explicit_sticky_rules_from_mirrored_upstream()
-> TestResult {
    let home = Path::new("/home");
    let rules = serde_json::to_vec(
        &json!({"schema_version":1,"standalone_manual_only":["alpha"],"plugins":{},"mirror_standalone_upstream_manual":true}),
    )?;
    let raw = serde_json::to_vec(
        &json!({"schema_version":1,"policy_hash":invocation::hash(&rules),"files":{}}),
    )?;
    let policy = applied(home, Some(&rules), Some(&raw))?;
    assert_eq!(
        intent(policy.declaration.as_ref(), "alpha", None)?,
        Intent::ExplicitManual
    );
    assert!(Intent::ExplicitManual.manual());
    assert_eq!(
        intent(policy.declaration.as_ref(), "beta", Some(false))?,
        Intent::Mirror {
            upstream_manual: false
        }
    );
    assert!(
        !Intent::Mirror {
            upstream_manual: false
        }
        .manual()
    );
    assert!(intent(policy.declaration.as_ref(), "beta", Some(true))?.manual());
    assert!(intent(policy.declaration.as_ref(), "beta", None).is_err());
    assert_eq!(
        intent(policy.declaration.as_ref(), "devnorm", Some(true))?,
        Intent::Unmanaged
    );
    assert_eq!(intent(None, "alpha", None)?, Intent::Unmanaged);
    assert!(applied(home, Some(&rules), None).is_err());
    assert!(applied(home, None, Some(&raw)).is_err());
    assert!(applied(home, Some(b"{"), Some(&raw)).is_err());
    assert!(applied(home, None, Some(b"{")).is_err());
    let mut declaration = policy.declaration.ok_or("declaration")?;
    declaration.mirror_standalone_upstream_manual = false;
    assert_eq!(intent(Some(&declaration), "beta", None)?, Intent::Unmanaged);
    assert!(applied(home, None, None)?.book.files.is_empty());
    Ok(())
}

#[test]
fn legacy_source_drift_retains_its_public_error_identity() {
    let error = drift();
    assert_eq!(error.code, "source_policy_drift");
    assert_eq!(error.exit, 3);
}
