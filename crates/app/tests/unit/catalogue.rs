#![cfg(test)]
use super::*;
use crate::{domain::*, test_support::*};
use serde_json::json;

#[test]
fn catalogue_sources_merge_without_overwriting_declared_owners() -> TestResult {
    let mut catalog = Catalog {
        schema_version: 1,
        packages: vec![package()],
    };
    add_ai(&mut catalog);
    let count = catalog.packages.len();
    add_ai(&mut catalog);
    assert_eq!(catalog.packages.len(), count);
    add_managers(Path::new("/cache"), &mut catalog);
    let count = catalog.packages.len();
    assert!(count > 8);
    add_managers(Path::new("/cache"), &mut catalog);
    assert_eq!(catalog.packages.len(), count);
    let receipt = Receipt {
        package: package(),
        scope: None,
        query: None,
        observation: Observation::Missing { evidence: None },
        operation_id: "OP-1".into(),
        updated_at: 1,
    };
    let mut other = receipt.clone();
    other.package.id = "new:demo".into();
    merge_receipts(
        &mut catalog,
        Receipts {
            schema_version: 1,
            installations: BTreeMap::from([("existing".into(), receipt), ("new".into(), other)]),
        },
    )?;
    assert_eq!(
        catalog
            .packages
            .iter()
            .filter(|p| p.id == "fixture:demo")
            .count(),
        1
    );
    assert!(catalog.packages.iter().any(|p| p.id == "new:demo"));
    assert!(
        merge_receipts(
            &mut catalog,
            Receipts {
                schema_version: 2,
                installations: BTreeMap::new()
            }
        )
        .is_err()
    );
    add_assets(
        &mut catalog,
        &json!({"assets":{"plain":{"kind":"github-skill","repository":"owner/repo"},"plugin":{"kind":"local-plugin"}}}),
    )?;
    assert!(
        catalog
            .packages
            .iter()
            .any(|p| p.id == "skill:plugin" && p.kind == Kind::Plugin)
    );
    let count = catalog.packages.len();
    add_assets(
        &mut catalog,
        &json!({"assets":{"plain":{"kind":"github-skill"}}}),
    )?;
    assert_eq!(catalog.packages.len(), count);
    assert!(add_assets(&mut catalog, &json!({})).is_err());
    assert!(add_assets(&mut catalog, &json!({"assets":{"bad":{}}})).is_err());
    let row = BTreeMap::from([
        ("bundle_id".into(), "com.demo".into()),
        ("version".into(), "1".into()),
        ("owner".into(), "vendor".into()),
        ("source".into(), "vendor".into()),
        ("uninstaller".into(), "native".into()),
    ]);
    assert_eq!(
        app_row(&row, Path::new("/Demo.app"))?.ok_or("app")?.id,
        "app:com.demo"
    );
    assert!(app_row(&BTreeMap::new(), Path::new("/Demo.app")).is_err());
    assert!(app_row(&row, Path::new("/")).is_err());
    Ok(())
}
