#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
fn member(id: &str, name: &str, repo: &str) -> Result<Member> {
    Ok(Member {
        id: id.into(),
        canonical_name: name.into(),
        repository: Repository::parse(repo)?,
        aliases: BTreeSet::from([format!("skill:{name}")]),
    })
}
#[test]
fn source_selection_is_explicit_unique_and_does_not_reject_valid_upgrade_revisions_as_sync_does()
-> TestResult {
    let members = [
        member("capability:a", "a", "owner/repo")?,
        member("asset:b", "b", "owner/repo")?,
    ];
    let groups = [SourceGroup {
        id: "source:bundle".into(),
        aliases: BTreeSet::from(["bundle".into()]),
        members: BTreeSet::from(["capability:a".into(), "asset:b".into()]),
    }];
    let request = Request::parse(
        Some("source:bundle"),
        &[],
        false,
        Some(&"A".repeat(40)),
        None,
    )?;
    assert_eq!(
        request.commit.as_ref().ok_or("commit")?.as_str(),
        "a".repeat(40)
    );
    assert_eq!(resolve(&request, &members, &groups)?.len(), 2);
    assert!(Request::parse(None, &[], false, None, None).is_err());
    let overlap = Request::parse(
        None,
        &["source:bundle".into(), "asset:b".into()],
        false,
        None,
        None,
    )?;
    assert_eq!(
        resolve(&overlap, &members, &groups)
            .err()
            .ok_or("overlap")?
            .code,
        "source_selection_overlap"
    );
    let mut different = members;
    different[1].repository = Repository::parse("other/repo")?;
    assert_eq!(
        resolve(&request, &different, &groups)
            .err()
            .ok_or("repo")?
            .code,
        "source_revision_cross_repository"
    );
    Ok(())
}
#[test]
fn qualified_ids_are_not_shadowed_by_display_aliases_and_duplicate_targets_are_rejected()
-> TestResult {
    let a = member("capability:a", "same", "owner/repo")?;
    let b = member("asset:same", "same", "other/repo")?;
    let group = SourceGroup {
        id: "source:group".into(),
        aliases: BTreeSet::from(["capability:a".into()]),
        members: BTreeSet::from([b.id.clone()]),
    };
    let explicit = Request::parse(Some("capability:a"), &[], false, None, None)?;
    assert_eq!(
        resolve(
            &explicit,
            &[a.clone(), b.clone()],
            std::slice::from_ref(&group)
        )?,
        BTreeSet::from([a.id.clone()])
    );
    let ambiguous = Request::parse(Some("same"), &[], false, None, None)?;
    assert!(resolve(&ambiguous, &[a.clone(), b.clone()], &[]).is_err());
    let all = Request::parse(None, &[], true, None, None)?;
    assert_eq!(
        resolve(&all, &[a, b], &[group]).err().ok_or("target")?.code,
        "source_target_overlap"
    );
    Ok(())
}

#[test]
fn source_selection_rejects_corrupt_members_groups_empty_groups_and_excessive_batches() -> TestResult
{
    let a = member("asset:a", "a", "owner/repo")?;
    let all = Request::parse(None, &[], true, None, None)?;
    assert!(Request::parse(Some("asset:a"), &[], false, Some("bad"), None).is_err());
    for mut values in [vec![a.clone(); 5001], vec![a.clone(), a.clone()]] {
        assert!(resolve(&all, &values, &[]).is_err());
        values.clear();
    }
    for variant in 0..3 {
        let mut bad = a.clone();
        match variant {
            0 => bad.canonical_name = "../a".into(),
            1 => bad.id = String::new(),
            _ => {
                bad.aliases.insert("bad\n".into());
            }
        }
        assert!(resolve(&all, &[bad], &[]).is_err());
    }
    let group = SourceGroup {
        id: "source:group".into(),
        aliases: BTreeSet::new(),
        members: BTreeSet::from([a.id.clone()]),
    };
    assert!(resolve(&all, std::slice::from_ref(&a), &vec![group.clone(); 5001]).is_err());
    for variant in 0..4 {
        let mut bad = group.clone();
        match variant {
            0 => bad.id = "other".into(),
            1 => {
                bad.members.insert("missing".into());
            }
            2 => {
                bad.aliases.insert("bad\n".into());
            }
            _ => (),
        }
        let groups = if variant == 3 {
            vec![bad.clone(), bad]
        } else {
            vec![bad]
        };
        assert!(resolve(&all, std::slice::from_ref(&a), &groups).is_err());
    }
    let mut empty = group;
    empty.members.clear();
    let explicit = Request::parse(Some("source:group"), &[], false, None, None)?;
    assert!(resolve(&explicit, std::slice::from_ref(&a), &[empty]).is_err());
    for keys in [vec![], vec!["\n".into()]] {
        let bad = Request {
            selection: Selection::Explicit(keys),
            commit: None,
            change_id: None,
        };
        assert!(resolve(&bad, std::slice::from_ref(&a), &[]).is_err());
    }
    let members = (0..129)
        .map(|n| member(&format!("asset:m{n}"), &format!("m{n}"), "owner/repo"))
        .collect::<Result<Vec<_>>>()?;
    assert!(resolve(&all, &members, &[]).is_err());
    let many = (0..5000)
        .map(|n| member(&format!("asset:m{n}"), &format!("m{n}"), "owner/repo"))
        .collect::<Result<Vec<_>>>()?;
    let references = many.iter().map(|m| m.id.clone()).collect::<BTreeSet<_>>();
    let groups = (0..5)
        .map(|n| SourceGroup {
            id: format!("source:g{n}"),
            aliases: BTreeSet::new(),
            members: references.clone(),
        })
        .collect::<Vec<_>>();
    assert!(resolve(&all, &many, &groups).is_err());
    Ok(())
}

#[test]
fn unqualified_member_and_group_aliases_remain_disjoint_from_source_qualified_names() -> TestResult
{
    let members = [member("capability:a", "a", "owner/repo")?];
    let groups = [SourceGroup {
        id: "source:bundle".into(),
        aliases: BTreeSet::from(["bundle".into()]),
        members: BTreeSet::from(["capability:a".into()]),
    }];
    for key in ["a", "skill:a", "bundle", "source:bundle", "capability:a"] {
        let request = Request::parse(Some(key), &[], false, None, None)?;
        assert_eq!(
            resolve(&request, &members, &groups)?,
            BTreeSet::from(["capability:a".into()])
        );
    }
    let request = Request::parse(Some("unknown"), &[], false, None, None)?;
    assert!(resolve(&request, &members, &groups).is_err());
    Ok(())
}
