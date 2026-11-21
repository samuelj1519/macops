#![cfg(test)]
use super::*;
use crate::test_support::TestResult;

#[test]
fn repository_identity_cannot_be_a_command_url_or_escaping_path() -> TestResult {
    let repo = Repository::parse("Owner/Repo.Name")?;
    assert_eq!(repo.key(), "owner/repo.name");
    assert_eq!(repo.url(), "https://github.com/owner/repo.name.git");
    for value in [
        "",
        "owner",
        "a/b/c",
        "/repo",
        "owner/",
        "./repo",
        "owner/..",
        "https://github.com/a/b",
        "a/b?x",
        "a/b#main",
        "a/b\n",
        "用户/repo",
    ] {
        assert!(Repository::parse(value).is_err(), "{value:?}");
    }
    assert!(Repository::parse(&format!("{}/repo", "a".repeat(101))).is_err());
    Ok(())
}
#[test]
fn object_ids_and_relative_components_have_one_unambiguous_representation() -> TestResult {
    let sha = "a".repeat(40);
    assert_eq!(ObjectId::parse(&sha)?.as_str(), sha);
    for value in [
        "a".repeat(39),
        "a".repeat(41),
        "A".repeat(40),
        "g".repeat(40),
        "0".repeat(40),
    ] {
        assert!(ObjectId::parse(&value).is_err());
    }
    assert_eq!(RelativePath::parse(".")?.parts().len(), 0);
    let path = RelativePath::parse("skills/示例 file")?;
    assert_eq!(path.parts()[1].as_str(), "示例 file");
    for value in [
        "", "/root", "../out", "a/./b", "a//b", "a/", "a\\b", "a\0b", ".git", ".GiT", "a\nb",
    ] {
        assert!(RelativePath::parse(value).is_err(), "{value:?}");
    }
    assert!(RelativePath::parse(&vec!["a"; 33].join("/")).is_err());
    assert!(Component::parse(&"x".repeat(256)).is_err());
    Ok(())
}
#[test]
fn tree_protocol_distinguishes_files_directories_links_and_submodules_without_reading_targets()
-> TestResult {
    assert_eq!(tree_entries(b"")?.len(), 0);
    let sha = "b".repeat(40);
    let data = format!("040000 tree {sha}\tskills\0")
        + &format!(
            "100644 blob {sha}\tSKILL.md\x00100755 blob {sha}\tscript\x00120000 blob {sha}\tlink\x00160000 commit {sha}\tsubmodule\0"
        );
    let rows = tree_entries(data.as_bytes())?;
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].kind, TreeKind::Directory);
    assert_eq!(rows[1].kind, TreeKind::File { executable: false });
    assert_eq!(rows[2].kind, TreeKind::File { executable: true });
    assert_eq!(rows[3].kind, TreeKind::Symlink);
    assert_eq!(rows[4].kind, TreeKind::Submodule);
    assert_eq!(rows[1].object.as_str(), sha);
    Ok(())
}
#[test]
fn malformed_tree_output_never_becomes_a_partial_inventory() {
    let sha = "c".repeat(40);
    for raw in [
        format!("100644 blob {sha}\tfile"),
        format!("100644 blob {sha}file\0"),
        format!("100644  blob {sha}\tfile\0"),
        format!("040000 blob {sha}\tfile\0"),
        format!("100600 blob {sha}\tfile\0"),
        "100644 blob bad\tfile\0".to_owned(),
        format!("100644 blob {sha}\t../out\0"),
        format!("100644 blob {sha}\tfile\0\0"),
        format!("100644 blob {sha}\tfile\x00100644 blob {sha}\tfile\0"),
    ] {
        assert!(tree_entries(raw.as_bytes()).is_err());
    }
    let mut name = format!("100644 blob {sha}\t").into_bytes();
    name.extend([0xff, 0]);
    assert!(tree_entries(&name).is_err());
    let mut header = vec![0xff];
    header.extend(format!(" blob {sha}\tname\0").bytes());
    assert!(tree_entries(&header).is_err());
    assert!(tree_entries(&vec![b'a'; 16 * 1024 * 1024 + 1]).is_err());
    let row = format!("100644 blob {sha}\tfile\0");
    let mut many = Vec::new();
    for n in 0..10001 {
        many.extend(row.replace("file", &n.to_string()).bytes());
    }
    assert!(tree_entries(&many).is_err());
}

#[test]
fn source_url_cannot_redirect_a_registered_github_identity_to_another_host_or_repository()
-> TestResult {
    let repo = Repository::parse("owner/repo")?;
    for url in [
        "https://github.com/owner/repo",
        "https://github.com/OWNER/Repo.git/",
        "https://github.com/owner/repo/tree/main/skills/a",
    ] {
        repo.verify_url(url)?;
    }
    for url in [
        "http://github.com/owner/repo",
        "https://github.com.evil/owner/repo",
        "https://github.com@evil/owner/repo",
        "https://github.com/other/repo",
        "https://github.com/owner/repo?token=x",
        "https://github.com/owner/repo/blob/main/a",
        "https://github.com/owner",
    ] {
        assert!(repo.verify_url(url).is_err());
    }
    Ok(())
}
#[test]
fn references_distinguish_tracking_names_and_explicit_commits_without_git_revision_expressions()
-> TestResult {
    assert_eq!(Reference::parse(None)?, Reference::Head);
    assert_eq!(Reference::parse(Some("HEAD"))?, Reference::Head);
    assert!(matches!(
        Reference::parse(Some(&"A".repeat(40)))?,
        Reference::Commit(_)
    ));
    assert_eq!(
        Reference::parse(Some("main"))?.patterns(),
        ["refs/heads/main", "refs/tags/main", "refs/tags/main^{}"]
    );
    assert_eq!(
        Reference::parse(Some("refs/heads/main"))?.patterns(),
        ["refs/heads/main"]
    );
    for value in [
        "",
        "--upload-pack=bad",
        "../out",
        "main~1",
        "main^{}",
        "refs//heads/main",
        "a@{b}",
        "/main",
        "main/",
        "a.lock",
        "a.",
        "a?b",
        "a b",
        "@",
    ] {
        assert!(Reference::parse(Some(value)).is_err(), "{value}");
    }
    Ok(())
}
#[test]
fn remote_advertisement_requires_one_exact_name_and_peels_only_its_annotated_tag() -> TestResult {
    let sha = "a".repeat(40);
    let tag = "b".repeat(40);
    assert_eq!(
        advertised_commit(
            &Reference::Head,
            format!("ref: refs/heads/main\tHEAD\n{sha}\tHEAD\n").as_bytes()
        )?
        .as_str(),
        sha
    );
    let reference = Reference::parse(Some("release"))?;
    assert_eq!(
        advertised_commit(
            &reference,
            format!("{tag}\trefs/tags/release\n{sha}\trefs/tags/release^{{}}\n").as_bytes()
        )?
        .as_str(),
        sha
    );
    assert!(
        advertised_commit(
            &reference,
            format!("{sha}\trefs/heads/release\n{tag}\trefs/tags/release\n").as_bytes()
        )
        .is_err()
    );
    for raw in [
        format!("{sha}\trefs/heads/other\n"),
        format!("{sha}\tHEAD\n{sha}\tHEAD\n"),
        "ref: bad\tHEAD\n".into(),
        format!("{sha}\tHEAD"),
        "invalid\tHEAD\n".into(),
    ] {
        assert!(advertised_commit(&Reference::Head, raw.as_bytes()).is_err());
    }
    let pin = Reference::parse(Some(&sha))?;
    assert_eq!(advertised_commit(&pin, b"")?.as_str(), sha);
    assert!(advertised_commit(&pin, b"unexpected").is_err());
    Ok(())
}

#[test]
fn pinned_and_fully_qualified_tags_and_remote_budget_symbol_proofs_are_complete() -> TestResult {
    let sha = "a".repeat(40);
    assert!(Reference::parse(Some(&"0".repeat(40))).is_err());
    assert_eq!(
        Reference::parse(Some(&sha))?.patterns(),
        std::slice::from_ref(&sha)
    );
    assert_eq!(
        Reference::parse(Some("refs/tags/v1"))?.patterns(),
        ["refs/tags/v1", "refs/tags/v1^{}"]
    );
    for bytes in [
        vec![b'x'; 65537],
        vec![255, b'\n'],
        b"no-tab\n".to_vec(),
        b"ref: refs/heads/a.lock\tHEAD\n".to_vec(),
        b"ref: refs/heads/main\tHEAD\nref: refs/heads/main\tHEAD\n".to_vec(),
        format!("{sha}\tHEAD\n").repeat(513).into_bytes(),
    ] {
        assert!(advertised_commit(&Reference::Head, &bytes).is_err());
    }
    let named = Reference::parse(Some("release"))?;
    for raw in [
        format!("ref: refs/heads/main\trefs/heads/release\n{sha}\trefs/tags/release\n"),
        format!("{sha}\trefs/heads/release\n{sha}\trefs/tags/release^{{}}\n"),
    ] {
        assert!(advertised_commit(&named, raw.as_bytes()).is_err());
    }
    Ok(())
}
