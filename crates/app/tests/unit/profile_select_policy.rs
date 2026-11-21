#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
#[test]
fn source_members_and_profile_configuration_are_distinct_explicit_write_units() -> TestResult {
    let one = split(Selection::Explicit(vec!["profile:mi".into()]))?;
    assert!(one.links.is_none());
    assert_eq!(
        one.profiles,
        BTreeSet::from([ConfigProfile::Claude(Profile::Mi)])
    );
    let mixed = split(Selection::Explicit(vec![
        "source:repo".into(),
        "profile:codex".into(),
    ]))?;
    assert_eq!(
        mixed.links,
        Some(Selection::Explicit(vec!["source:repo".into()]))
    );
    assert!(mixed.profiles.contains(&ConfigProfile::Codex));
    assert_eq!(split(Selection::All)?.profiles.len(), 5);
    for keys in [
        vec![],
        vec!["profile:mi".into(), "profile:mi".into()],
        vec!["profile:default".into()],
    ] {
        assert!(split(Selection::Explicit(keys)).is_err());
    }
    Ok(())
}
