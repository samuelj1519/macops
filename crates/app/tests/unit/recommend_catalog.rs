use super::*;
use crate::test_support::TestResult;

#[test]
fn recommendation_source_labels_omit_embedded_and_query_credentials() {
    assert_eq!(
        source_label(concat!(
            "https://reader:",
            "fixture-private",
            "@",
            "github.com/owner/repo"
        )),
        "待核对"
    );
    assert_eq!(
        source_label("https://github.com/owner/repo?key=fixture-private#fragment"),
        "https://github.com/owner/repo"
    );
}

#[test]
fn public_brew_evidence_does_not_select_another_tap_with_the_same_formula_name() -> TestResult {
    let mut package = crate::test_support::package();
    let mut query = crate::test_support::brew::query()?;
    query.selection.name = "jq".into();
    query.key.full_name = "jq".into();
    let advised = Candidate::Brew {
        name: "jq".into(),
        cask: false,
    };
    package.provider = Provider::BrewFormula {
        query: Box::new(query.clone()),
        recipe_ref: None,
    };
    assert!(matches_public_channel(&package, &advised));
    assert_eq!(
        preview_resource(&package).ok_or("expected preview")?["entry"],
        "install"
    );
    query.selection.tap = "custom/tap".into();
    query.key.full_name = "custom/tap/jq".into();
    package.provider = Provider::BrewFormula {
        query: Box::new(query),
        recipe_ref: None,
    };
    assert!(!matches_public_channel(&package, &advised));
    assert!(matches_public_channel(
        &package,
        &Candidate::Brew {
            name: "custom/tap/jq".into(),
            cask: false
        }
    ));
    Ok(())
}
