#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::{fs, os::unix::fs::PermissionsExt};
#[test]
fn private_materialization_preserves_binary_empty_directories_and_modes_without_overwriting()
-> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))?;
    let parent = Directory::open(temp.path())?;
    let payload = Payload {
        entries: std::collections::BTreeMap::from([
            (
                "SKILL.md".into(),
                Entry::File {
                    mode: 0o644,
                    bytes: b"body".to_vec(),
                },
            ),
            ("empty".into(), Entry::Directory { mode: 0o700 }),
            ("scripts".into(), Entry::Directory { mode: 0o755 }),
            (
                "scripts/run".into(),
                Entry::File {
                    mode: 0o755,
                    bytes: vec![0, 255, 1],
                },
            ),
        ]),
    };
    let (slot, tree) = materialize(&parent, "candidate", &payload, 0o755)?;
    assert_eq!(
        tree.shape(),
        payload.shape(nix::unistd::geteuid().as_raw(), 0o755)
    );
    assert_eq!(fs::read(slot.path().join("scripts/run"))?, vec![0, 255, 1]);
    assert_eq!(
        fs::metadata(slot.path().join("empty"))?
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    assert!(materialize(&parent, "candidate", &payload, 0o755).is_err());
    assert_eq!(crate::source_tree::observe(&slot)?.as_ref(), Some(&tree));
    assert!(materialize(&parent, "../outside", &payload, 0o755).is_err());
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755))?;
    assert!(materialize(&parent, "not-private", &payload, 0o755).is_err());
    assert!(!temp.path().join("not-private").exists());
    Ok(())
}
#[test]
fn replaced_private_root_is_not_reopened_and_written_as_an_unrelated_directory() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let path = temp.path().join("candidate");
    fs::create_dir(&path)?;
    let root = Directory::open(&path)?;
    let moved = temp.path().join("retained");
    fs::rename(&path, &moved)?;
    fs::create_dir(&path)?;
    assert!(
        write_entry(
            &root,
            "new",
            &Entry::File {
                mode: 0o600,
                bytes: b"candidate".to_vec()
            }
        )
        .is_err()
    );
    assert!(!path.join("new").exists());
    assert!(!moved.join("new").exists());
    Ok(())
}
fn object(repo: &std::path::Path, args: &[&str], bytes: &[u8]) -> Result<ObjectId> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("/usr/bin/git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", repo.parent().ok_or_else(invalid)?.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("--git-dir")
        .arg(repo)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().ok_or_else(invalid)?.write_all(bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(invalid());
    }
    ObjectId::parse(
        std::str::from_utf8(&output.stdout)
            .map_err(|_| invalid())?
            .trim_end_matches('\n'),
    )
}
#[test]
fn selected_git_objects_produce_exact_binary_payload_and_root_licenses_without_checkout()
-> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let path = temp.path().join("fetch");
    let git = Git::create_bound(
        &path,
        &crate::policy::source_git::Repository::parse("owner/repo")?,
    )?;
    let repo = path.join("objects.git");
    let skill = object(
        &repo,
        &["hash-object", "-w", "--stdin"],
        b"---\nname: demo\n---\nbody\n",
    )?;
    let binary = object(&repo, &["hash-object", "-w", "--stdin"], &[0, 255, 1])?;
    let license = object(&repo, &["hash-object", "-w", "--stdin"], b"LICENSE text")?;
    let empty = object(&repo, &["mktree", "-z"], b"")?;
    let scripts = object(
        &repo,
        &["mktree", "-z"],
        format!("100755 blob {}\trun\0", binary.as_str()).as_bytes(),
    )?;
    let selected = object(
        &repo,
        &["mktree", "-z"],
        format!(
            "100644 blob {}\tSKILL.md\x00040000 tree {}\tempty\x00040000 tree {}\tscripts\0",
            skill.as_str(),
            empty.as_str(),
            scripts.as_str()
        )
        .as_bytes(),
    )?;
    let skills = object(
        &repo,
        &["mktree", "-z"],
        format!("040000 tree {}\tdemo\0", selected.as_str()).as_bytes(),
    )?;
    let root = object(
        &repo,
        &["mktree", "-z"],
        format!(
            "100644 blob {}\tLICENSE\x00040000 tree {}\tskills\0",
            license.as_str(),
            skills.as_str()
        )
        .as_bytes(),
    )?;
    let rules = Rules::declared(
        &serde_json::json!({"kind":"github-skill","repository":"owner/repo","path":"skills/demo"}),
    )?;
    let prepared = read(&git, &root, &rules)?;
    assert_eq!(prepared.skill_tree, selected);
    assert_eq!(prepared.repository, rules.repository);
    assert_eq!(prepared.source_path, rules.path);
    let mut wrong = rules.clone();
    wrong.repository = crate::policy::source_git::Repository::parse("other/repo")?;
    assert!(read(&git, &root, &wrong).is_err());
    assert_payload_contents(&prepared);
    assert!(!path.join("SKILL.md").exists());
    let target = temp.path().join("stage");
    fs::create_dir(&target)?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
    let (slot, _) = materialize(
        &Directory::open(&target)?,
        "payload",
        &prepared.payload,
        0o755,
    )?;
    assert_eq!(fs::read(slot.path().join("scripts/run"))?, [0, 255, 1]);
    let bad = object(
        &repo,
        &["mktree", "-z"],
        format!(
            "100644 blob {}\tSKILL.md\x00120000 blob {}\tlink\0",
            skill.as_str(),
            binary.as_str()
        )
        .as_bytes(),
    )?;
    let native = Rules::native(
        crate::policy::source_git::Repository::parse("owner/repo")?,
        RelativePath::parse(".")?,
    );
    assert!(read(&git, &bad, &native).is_err());
    assert!(read(&git, &skill, &native).is_err());
    Ok(())
}

fn assert_payload_contents(prepared: &Prepared) {
    assert_eq!(
        prepared.payload.entries["scripts/run"],
        Entry::File {
            mode: 0o755,
            bytes: vec![0, 255, 1]
        }
    );
    assert_eq!(
        prepared.payload.entries["LICENSE"],
        Entry::File {
            mode: 0o644,
            bytes: b"LICENSE text".to_vec()
        }
    );
    assert!(matches!(
        prepared.payload.entries["empty"],
        Entry::Directory { mode: 0o755 }
    ));
}
