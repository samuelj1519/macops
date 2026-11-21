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
