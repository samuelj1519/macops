#![cfg(test)]
use super::*;
use crate::{
    cli::Cli,
    policy::source_payload::{Entry, Payload},
    test_support::TestResult,
};
use clap::Parser;
use std::fs;

fn fixture() -> std::result::Result<
    (
        tempfile::TempDir,
        Context,
        Recipe,
        crate::source_payload::Prepared,
    ),
    Box<dyn std::error::Error>,
> {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "list"])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let recipe: Recipe = crate::policy::native_skills::decode(&serde_json::to_vec(
        &serde_json::json!({
        "schemaVersion":1,"id":"writing-dna-skill","owner":"skills-cli","cliVersion":"1.9.0",
        "source":{"repository":"larashero3-dotcom/writing-dna-skill","commit":"a".repeat(40),"skillDirectory":".","memberName":"writing-dna-skill","trackingRef":null},
        "installName":"writing-dna-skill","scope":"global","clientScope":["codex"],"copy":true,"manualOnly":true,"licenseFiles":[],
        "derived":{"kind":"github-skill","repository":"larashero3-dotcom/writing-dna-skill","path":".","manual_only":true,"exclude":["skills/lieflat-less-ai-tone"],"edits":[{"path":"SKILL.md","old":"old prose","new":"new prose"}]},
        "runner":{"node":"/fixture/node","entry":"/fixture/skills/bin/cli.mjs","sha256":"f".repeat(64)}}),
    )?)?;
    let mut payload = Payload {
        entries: BTreeMap::from([(
            "SKILL.md".into(),
            Entry::File {
                mode: 0o644,
                bytes: b"---\nname: writing-dna-skill\n---\nold prose\n".to_vec(),
            },
        )]),
    };
    let rules = Rules::declared(recipe.derived.as_ref().ok_or("rules")?)?;
    payload.transform(
        &rules,
        &[(
            "LICENSE".into(),
            Entry::File {
                mode: 0o644,
                bytes: b"license".to_vec(),
            },
        )],
    )?;
    let source = crate::source_payload::Prepared {
        repository: Repository::parse(&recipe.source.repository)?,
        source_path: RelativePath::parse(".")?,
        skill_tree: ObjectId::parse(&"b".repeat(40))?,
        payload,
    };
    Ok((temp, ctx, recipe, source))
}

#[test]
fn scratch_repository_is_private_bound_and_removed_independently_of_nearby_cache() -> TestResult {
    let (_temp, ctx, mut recipe, _) = fixture()?;
    let (scratch, git, bound) = repository(&ctx, &recipe)?;
    let parent = ctx.cache.join("native-skills/source");
    let path = scratch.path().to_path_buf();
    let neighbor = parent.join("unrelated");
    fs::write(&neighbor, b"keep")?;
    assert_eq!(path.parent(), Some(parent.as_path()));
    for private in [&parent, &path, &path.join("git/objects.git")] {
        assert_eq!(fs::metadata(private)?.permissions().mode() & 0o7777, 0o700);
    }
    assert_eq!(bound, Repository::parse(&recipe.source.repository)?);
    git.require_repository(&bound)?;
    assert!(git.storage_usage()? > 0);
    drop(git);
    drop(scratch);
    assert!(!path.exists());

    recipe.source.repository = "../repo".into();
    assert!(repository(&ctx, &recipe).is_err());
    assert_eq!(fs::read_dir(&parent)?.count(), 1);
    assert_eq!(fs::read(neighbor)?, b"keep");
    Ok(())
}

#[test]
fn writing_owned_artifact_is_private_immutable_and_reusable_with_native_local_source_provenance()
-> TestResult {
    let (_temp, ctx, recipe, source) = fixture()?;
    let prepared = derived_artifact(&ctx, &recipe, &source, &source.skill_tree)?;
    let payload = prepared.local.as_ref().ok_or("local")?;
    assert_eq!(
        fs::metadata(payload.parent().ok_or("artifact")?)?
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    assert_eq!(fs::metadata(payload)?.permissions().mode() & 0o7777, 0o700);
    prepared.verify()?;
    owned_local(&ctx, &recipe, payload, true)?;
    let second = derived_artifact(&ctx, &recipe, &source, &source.skill_tree)?;
    assert_eq!(second.local, prepared.local);
    assert_eq!(second.local_digest, prepared.local_digest);
    let parent = files::private(&ctx.home.join(".agents/skills"))?;
    crate::source_payload::materialize(&parent, &recipe.install_name, &source.payload, 0o755)?;
    installed_derived(&ctx, &recipe, payload)?;
    fs::write(
        ctx.home
            .join(".agents/skills")
            .join(&recipe.install_name)
            .join("SKILL.md"),
        b"foreign",
    )?;
    assert!(installed_derived(&ctx, &recipe, payload).is_err());
    let mut changed = recipe.clone();
    changed.source.commit = "c".repeat(40);
    assert!(owned_local(&ctx, &changed, payload, true).is_err());
    owned_local(&ctx, &changed, payload, false)?;
    assert!(owned_local(&ctx, &recipe, &ctx.data.join("foreign/payload"), true).is_err());
    fs::write(payload.join("SKILL.md"), b"foreign artifact")?;
    assert!(prepared.verify().is_err());
    assert!(owned_local(&ctx, &recipe, payload, true).is_err());
    assert!(derived_artifact(&ctx, &recipe, &source, &source.skill_tree).is_err());
    Ok(())
}
#[test]
fn missing_conflicting_manifest_and_plain_native_recipe_do_not_create_another_publisher()
-> TestResult {
    let (_temp, ctx, mut recipe, source) = fixture()?;
    let prepared = derived_artifact(&ctx, &recipe, &source, &source.skill_tree)?;
    let payload = prepared.local.as_ref().ok_or("payload")?;
    fs::write(
        payload.parent().ok_or("artifact")?.join("manifest.json"),
        b"{}",
    )?;
    assert!(derived_artifact(&ctx, &recipe, &source, &source.skill_tree).is_err());
    assert!(owned_local(&ctx, &recipe, payload, true).is_err());
    recipe.derived = None;
    let direct = prepare(&ctx, &recipe)?;
    assert!(direct.local.is_none());
    direct.verify()?;
    let wrong = Prepared {
        local: Some(ctx.data.join("absent/payload")),
        local_digest: None,
        licenses: BTreeMap::new(),
        upstream_tree: String::new(),
    };
    assert!(wrong.verify().is_err());
    recipe.license_files = vec!["LICENSE".into()];
    recipe.source.repository = "../repo".into();
    assert!(prepare(&ctx, &recipe).is_err());
    assert!(candidate(&ctx, &recipe).is_err());
    Ok(())
}

fn git_object(
    repo: &std::path::Path,
    args: &[&str],
    bytes: &[u8],
) -> std::result::Result<ObjectId, Box<dyn std::error::Error>> {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let mut child = Command::new("/usr/bin/git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", repo.parent().ok_or("parent")?)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("--git-dir")
        .arg(repo)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().ok_or("stdin")?.write_all(bytes)?;
    let output = child.wait_with_output()?;
    assert!(output.status.success());
    Ok(ObjectId::parse(
        std::str::from_utf8(&output.stdout)?.trim(),
    )?)
}
#[test]
fn candidate_fingerprints_include_only_the_bound_subtree_licenses_and_declared_edits() -> TestResult
{
    let (temp, ctx, mut recipe, _) = fixture()?;
    let path = temp.path().join("candidate-git");
    let git = Git::create_bound(&path, &Repository::parse(&recipe.source.repository)?)?;
    let repo = path.join("objects.git");
    let skill = git_object(
        &repo,
        &["hash-object", "-w", "--stdin"],
        b"---\nname: writing-dna-skill\n---\nold prose\n",
    )?;
    let skill_line = format!("100644 blob {}\tSKILL.md\0", skill.as_str());
    let subtree = git_object(&repo, &["mktree", "-z"], skill_line.as_bytes())?;
    let license = git_object(&repo, &["hash-object", "-w", "--stdin"], b"license")?;
    let derived_tree = git_object(
        &repo,
        &["mktree", "-z"],
        format!("{skill_line}100644 blob {}\tLICENSE\0", license.as_str()).as_bytes(),
    )?;
    let (derived, selected) = candidate_identity(&git, &derived_tree, &recipe)?;
    assert_eq!(selected, derived_tree.as_str());
    let prepared = prepare_fetched(&ctx, &recipe, &git, &derived_tree)?;
    assert_eq!(prepared.upstream_tree, derived_tree.as_str());
    let payload = prepared.local.ok_or("derived payload")?;
    assert!(fs::read_to_string(payload.join("SKILL.md"))?.contains("new prose"));
    assert_eq!(fs::read(payload.join("LICENSE"))?, b"license");
    let mut changed = recipe.clone();
    changed.derived.as_mut().ok_or("derived")?["edits"][0]["new"] = "different prose".into();
    assert_ne!(
        candidate_identity(&git, &derived_tree, &changed)?.0,
        derived
    );

    recipe.derived = None;
    recipe.source.skill_directory = "selected".into();
    recipe.license_files = vec!["LICENSE".into()];
    let candidate =
        |license: &[u8]| -> std::result::Result<(String, String), Box<dyn std::error::Error>> {
            let license = git_object(&repo, &["hash-object", "-w", "--stdin"], license)?;
            let root = git_object(
                &repo,
                &["mktree", "-z"],
                format!(
                    "100644 blob {}\tLICENSE\x00040000 tree {}\tselected\0",
                    license.as_str(),
                    subtree.as_str()
                )
                .as_bytes(),
            )?;
            let prepared = prepare_fetched(&ctx, &recipe, &git, &root)?;
            assert!(prepared.local.is_none());
            assert_eq!(prepared.upstream_tree, subtree.as_str());
            assert!(prepared.licenses.contains_key("LICENSE"));
            Ok(candidate_identity(&git, &root, &recipe)?)
        };
    let (first, first_tree) = candidate(b"license one")?;
    let (second, second_tree) = candidate(b"license two")?;
    assert_ne!(first, second);
    assert_eq!(first_tree, subtree.as_str());
    assert_eq!(first_tree, second_tree);
    assert!(candidate_identity(&git, &subtree, &recipe).is_err());
    Ok(())
}

#[test]
fn license_metadata_reader_uses_only_selected_git_blobs_and_rejects_duplicate_flattened_names()
-> TestResult {
    let (temp, _ctx, mut recipe, _source) = fixture()?;
    let path = temp.path().join("read-git");
    let git = Git::create_bound(&path, &Repository::parse(&recipe.source.repository)?)?;
    let repo = path.join("objects.git");
    let license = git_object(&repo, &["hash-object", "-w", "--stdin"], b"license body")?;
    let nested = git_object(
        &repo,
        &["mktree", "-z"],
        format!("100644 blob {}\tLICENSE\0", license.as_str()).as_bytes(),
    )?;
    let tree = git_object(
        &repo,
        &["mktree", "-z"],
        format!(
            "100644 blob {}\tLICENSE\x00040000 tree {}\tnested\0",
            license.as_str(),
            nested.as_str()
        )
        .as_bytes(),
    )?;
    recipe.license_files = vec!["LICENSE".into()];
    assert_eq!(
        license_bytes(&git, &tree, &recipe)?.get("LICENSE"),
        Some(&b"license body".to_vec())
    );
    recipe.license_files.push("nested/LICENSE".into());
    assert!(license_bytes(&git, &tree, &recipe).is_err());
    recipe.license_files = vec![".".into()];
    assert!(license_bytes(&git, &tree, &recipe).is_err());
    Ok(())
}
