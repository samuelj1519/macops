#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::io::Write;
#[test]
fn git_object_reads_are_binary_exact_and_do_not_checkout_or_run_source_hooks() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let git = Git::create(&temp.path().join("session"))?;
    let bytes = [0, 255, b'A', 0];
    let mut args = vec![
        std::ffi::OsString::from("--git-dir"),
        git.repo.as_os_str().to_owned(),
    ];
    args.extend(["hash-object", "-w", "--stdin"].map(std::ffi::OsString::from));
    let mut command = git.command(&args)?;
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = command.spawn()?;
    child.stdin.take().ok_or("stdin")?.write_all(&bytes)?;
    let output = child.wait_with_output()?;
    assert!(output.status.success());
    let id = ObjectId::parse(std::str::from_utf8(&output.stdout)?.trim())?;
    assert_eq!(git.blob(&id)?, bytes);
    assert!(git.tree(&id).is_err());
    let args = [
        "--git-dir".as_ref(),
        git.repo.as_os_str(),
        "mktree".as_ref(),
    ]
    .map(std::ffi::OsString::from);
    let tree_output = git.run(&args, 1024)?;
    let tree = ObjectId::parse(std::str::from_utf8(&tree_output)?.trim())?;
    assert_eq!(git.tree(&tree)?.len(), 0);
    assert!(git.blob(&tree).is_err());
    assert!(!git.root.path.join("SKILL.md").exists());
    assert!(git.home.read_dir()?.next().is_none());
    Ok(())
}
#[test]
fn git_commands_replace_all_inherited_environment_and_use_a_private_configuration() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let git = Git::create(&temp.path().join("session"))?;
    let command = git.command(&[])?;
    let env: std::collections::BTreeMap<_, _> = command
        .get_envs()
        .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned())))
        .collect();
    assert_eq!(
        env.get(OsStr::new("HOME")),
        Some(&git.home.as_os_str().to_owned())
    );
    assert_eq!(
        env.get(OsStr::new("GIT_ALLOW_PROTOCOL")),
        Some(&"https".into())
    );
    assert!(!env.contains_key(OsStr::new("SSH_AUTH_SOCK")));
    assert!(!env.contains_key(OsStr::new("GH_TOKEN")));
    assert!(!env.contains_key(OsStr::new("GIT_CONFIG_PARAMETERS")));
    assert_eq!(command.get_current_dir(), Some(git.root.path.as_path()));
    assert!(Git::create(&git.root.path).is_err());
    Ok(())
}
#[test]
fn private_git_endpoint_modes_config_bytes_and_directory_identity_are_frozen() -> TestResult {
    for variant in 0..4 {
        let temp = tempfile::tempdir_in("/private/tmp")?;
        let git = Git::create(&temp.path().join("session"))?;
        match variant {
            0 => fs::set_permissions(&git.home, fs::Permissions::from_mode(0o755))?,
            1 => fs::write(&git.empty, b"[alias]\nmalicious = !ignored\n")?,
            2 => {
                fs::rename(&git.hooks, git.root.path.join("old-hooks"))?;
                fs::create_dir(&git.hooks)?;
                fs::set_permissions(&git.hooks, fs::Permissions::from_mode(0o700))?;
            }
            _ => fs::write(
                git.repo.join("config"),
                b"[core]\n hooksPath = /untrusted\n",
            )?,
        }
        assert!(git.command(&[]).is_err());
    }
    Ok(())
}
#[test]
fn private_git_configuration_is_bound_once_to_one_public_repository_and_reuse_keeps_its_identity()
-> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let repository = Repository::parse("owner/repo")?;
    let mut git = Git::create_bound(&temp.path().join("session"), &repository)?;
    git.require_repository(&repository)?;
    let before = git.repo_witness.clone().ok_or("config")?;
    let text = fs::read_to_string(git.repo.join("config"))?;
    assert!(text.contains("repositoryformatversion = 1"));
    assert!(text.contains("url = https://github.com/owner/repo.git"));
    assert!(text.contains("promisor = true"));
    assert!(text.contains("partialclonefilter = blob:none"));
    git.bind(&repository)?;
    assert_eq!(git.repo_witness.as_ref(), Some(&before));
    let other = Repository::parse("other/repo")?;
    assert!(git.bind(&other).is_err());
    assert!(git.require_repository(&other).is_err());
    assert!(
        git.resolve(&other, &crate::policy::source_git::Reference::Head)
            .is_err()
    );
    assert_eq!(fs::read_to_string(git.repo.join("config"))?, text);
    Ok(())
}

fn object_with_input(git: &Git, arguments: &[&str], input: &[u8]) -> Result<ObjectId> {
    let mut args = vec![
        std::ffi::OsString::from("--git-dir"),
        git.repo.as_os_str().to_owned(),
    ];
    args.extend(arguments.iter().map(std::ffi::OsString::from));
    let mut command = git.command(&args)?;
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    command
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.org")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.org");
    let mut child = command.spawn()?;
    child.stdin.take().ok_or_else(invalid)?.write_all(input)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(invalid());
    }
    ObjectId::parse(
        std::str::from_utf8(&output.stdout)
            .map_err(|_| invalid())?
            .trim(),
    )
}

#[test]
fn frozen_full_mirror_proves_actual_git_manifest_and_binary_file_bytes() -> TestResult {
    use crate::codex_install_source::{Offer, sha, verified_mirror_contents};
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let git = Git::create(&temp.path().join("source"))?;
    let manifest = br#"{"name":"demo","version":"1.0.0"}"#;
    let market = br#"{"name":"market","plugins":[{"name":"demo","source":{"source":"url","url":"https://github.com/owner/repo.git","ref":"main"}}]}"#;
    let portable = br#"{"name":"other client","description":"unrelated"}"#;
    let body = b"binary\0payload";
    let blob = |bytes: &[u8]| object_with_input(&git, &["hash-object", "-w", "--stdin"], bytes);
    let tree = |bytes: &str| object_with_input(&git, &["mktree"], bytes.as_bytes());
    let native = tree(&format!(
        "100644 blob {}\tplugin.json\n",
        blob(manifest)?.as_str()
    ))?;
    let catalog = tree(&format!(
        "100644 blob {}\tmarketplace.json\n",
        blob(market)?.as_str()
    ))?;
    let agents = tree(&format!("040000 tree {}\tplugins\n", catalog.as_str()))?;
    let root = |portable: &[u8]| -> Result<ObjectId> {
        tree(&format!(
            "040000 tree {}\t.agents\n040000 tree {}\t.codex-plugin\n100644 blob {}\tplugin.json\n100755 blob {}\ttest-data\n",
            agents.as_str(),
            native.as_str(),
            blob(portable)?.as_str(),
            blob(body)?.as_str()
        ))
    };
    let payload = root(portable)?;
    let commit = object_with_input(
        &git,
        &["commit-tree", payload.as_str(), "-m", "fixture"],
        b"",
    )?;
    fs::write(
        git.repo.join("FETCH_HEAD"),
        format!("{}\n", commit.as_str()),
    )?;
    let (verified, repo) = git.pin_fetched_mirror(&commit, "main")?;
    assert_eq!(verified, payload);
    assert_eq!(repo, git.repo);
    assert_eq!(
        git.object_command(&["symbolic-ref", "HEAD"], 1024)?,
        b"refs/heads/main\n"
    );
    assert!(
        git.pin_fetched_mirror(&ObjectId::parse(&"b".repeat(40))?, "main")
            .is_err()
    );
    assert!(git.mirror_blobs(&[], 0).is_err());
    let repo = Repository::parse("owner/repo")?;
    assert!(git.fetch_full_mirror(&repo, &commit, "HEAD").is_err());
    assert!(
        git.fetch_full_mirror(&repo, &commit, "refs/heads/main")
            .is_err()
    );
    let mut offer = Offer::parse(&serde_json::json!({
        "repository":repo.key(),"marketplace":"market","plugin":"demo","declaredVersion":"1.0.0",
        "marketplaceCommit":commit.as_str(),"payloadCommit":commit.as_str(),"payloadRef":"main",
        "marketplaceSha256":sha(market),"pluginManifestSha256":sha(manifest),
    }))?;
    let files = verified_mirror_contents(&git, &verified, &offer)?;
    assert_eq!(files.len(), 4);
    assert!(files.contains(&("test-data".into(), true, sha(body))));
    let (_, staged) = crate::codex_install_source::acquire_using(
        &offer,
        &temp.path().join("independent-mirror"),
        |destination, repository, expected, branch| {
            assert_eq!(expected, &commit);
            assert_eq!(branch, "main");
            let status = Command::new("/bin/cp")
                .arg("-R")
                .arg(git.repo.join("objects/."))
                .arg(destination.repo.join("objects"))
                .status()?;
            assert!(status.success());
            fs::write(
                destination.repo.join("FETCH_HEAD"),
                format!("{}\n", expected.as_str()),
            )?;
            let (tree, _) =
                destination.fetch_full_mirror_using(repository, expected, branch, |_, argv| {
                    assert_eq!(argv[0], "fetch");
                    assert_eq!(argv[5], "https://github.com/owner/repo.git");
                    assert!(argv.contains(&"--no-recurse-submodules"));
                    Ok(Vec::new())
                })?;
            Ok(tree)
        },
    )?;
    assert_eq!(staged, files);
    let competing = root(br#"{"$schema":"https://agent-plugins.org/schemas/1.0.0/plugin.schema.json","name":"demo","version":"2.0.0"}"#)?;
    assert!(verified_mirror_contents(&git, &competing, &offer).is_err());
    offer.marketplace_sha256 = "1".repeat(64);
    assert!(verified_mirror_contents(&git, &verified, &offer).is_err());
    Ok(())
}

#[test]
fn full_mirror_storage_budget_rejects_excess_bytes_and_deep_directories() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let mut git = Git::create(&temp.path().join("source"))?;
    let budget = git.storage_usage()?;
    git.set_storage_budget(budget + 100)?;
    let large = git.repo.join("fixture-data");
    fs::write(&large, vec![0_u8; 1024])?;
    let error = git.storage_usage().err().ok_or("storage over budget")?;
    assert_eq!(error.code, "source_storage_budget");
    fs::remove_file(large)?;
    let mut deep = git.repo.clone();
    for _ in 0..35 {
        deep.push("level");
    }
    fs::create_dir_all(deep)?;
    assert!(git.storage_usage().is_err());
    Ok(())
}
