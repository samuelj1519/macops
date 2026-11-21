#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
#[test]
fn harness_roots_are_frozen_separately_without_changing_unrelated_installation_scopes() -> TestResult
{
    let mut warnings = vec![];
    let captured = HarnessRoots::from_values(
        Some("/private/tmp/claude-profile".into()),
        Some("/private/tmp/codex-profile".into()),
        false,
        &mut warnings,
    )?;
    assert_eq!(
        captured.claude.as_deref(),
        Some(std::path::Path::new("/private/tmp/claude-profile"))
    );
    assert_eq!(
        captured.codex.as_deref(),
        Some(std::path::Path::new("/private/tmp/codex-profile"))
    );
    let isolated = HarnessRoots::from_values(captured.claude, captured.codex, true, &mut warnings)?;
    assert!(isolated.claude.is_none() && isolated.codex.is_none());
    assert!(
        HarnessRoots::from_values(Some("relative".into()), None, false, &mut warnings).is_err()
    );
    assert!(
        HarnessRoots::from_values(Some(PathBuf::new()), None, false, &mut warnings)?
            .claude
            .is_none()
    );
    assert!(
        HarnessRoots::from_values(
            None,
            Some("/Users/example/job/excluded".into()),
            false,
            &mut warnings
        )
        .is_err()
    );
    Ok(())
}
