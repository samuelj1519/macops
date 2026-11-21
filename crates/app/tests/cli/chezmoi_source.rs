use super::{Fixture, TestResult, assert_code};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    process::{Command, Output},
};

struct SourceFixture {
    base: Fixture,
    source: PathBuf,
    config: PathBuf,
    state: PathBuf,
}

impl SourceFixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let base = Fixture::new()?;
        fs::set_permissions(base.root.path(), fs::Permissions::from_mode(0o700))?;
        let source = base.home.join("dotfiles");
        let config = base.home.join("native-config/chezmoi.toml");
        let state = base.home.join(".local/state/macops");
        fs::create_dir(&source)?;
        fs::create_dir_all(config.parent().ok_or("config parent")?)?;
        fs::write(&config, "")?;
        base.script(
            "chezmoi",
            r#"src="$HOME/dotfiles"; root="$HOME"; cfg="$HOME/native-config/chezmoi.toml"
action=""; selected=""
while [ "$#" -gt 0 ]; do
 case "$1" in
 --source) src="$2"; shift 2;;
 --destination) root="$2"; shift 2;;
 --config) cfg="$2"; shift 2;;
 --cache|--persistent-state) shift 2;;
 --version) action=version; shift;;
 dump-config|source-path|target-path|execute-template|managed|status|verify|add|forget) action="$1";shift;;
 --) shift; selected="$1"; break;;
 *) shift;;
 esac
done
if [ "$HOME" = "$MACOPS_TEST_HOME" ]; then
 printf '%s
' "$action" >> "$HOME/native-calls"
 [ "$action" != add ] && [ "$action" != forget ] || exit 29
fi
emit_mapping() {
  printf '%s"%s":{"absolute":"%s/%s","sourceAbsolute":"%s/%s","sourceRelative":"%s"}' "$separator" "$1" "$root" "$1" "$src" "$2" "$2"
  separator=','
}
selected_source() {
  case "$selected" in
   "$root/.gitconfig") printf '%s/dot_gitconfig' "$src";;
   "$root/.config/sample/settings.toml") printf '%s/dot_config/sample/private_settings.toml' "$src";;
   *) return 30;;
  esac
}
case "$action" in
 version) printf 'chezmoi version v2.73.3
';;
 dump-config) printf '{"sourceDir":"%s","destDir":"%s","status":{"exclude":[]},"verify":{"exclude":[]}}
' "$src" "$root";;
 source-path) printf '%s
' "$src";;
 target-path) printf '%s
' "$root";;
 execute-template) printf '{"configFile":"%s","cache":"%s/cache","configuredState":"%s/native.db","resolvedDefault":"%s/native.db"}
' "$cfg" "$HOME" "$HOME" "$HOME";;
 add)
   [ "$root" = "$HOME" ] || exit 31
   destination="$(selected_source)" || exit 30
   /bin/cp "$selected" "$destination" ;;
 forget)
   [ "$root" = "$HOME" ] || exit 31
   removed="$(selected_source)" || exit 30
   /bin/rm "$removed";;
 verify) /usr/bin/cmp "$selected" "$(selected_source)";;
 status) :;;
 managed)
   printf '{'
   separator=''
   if [ -f "$src/dot_gitconfig" ]; then emit_mapping '.gitconfig' 'dot_gitconfig'; fi
   if [ -d "$src/dot_config" ]; then emit_mapping '.config' 'dot_config'; fi
   if [ -d "$src/dot_config/sample" ]; then emit_mapping '.config/sample' 'dot_config/sample'; fi
   if [ -f "$src/dot_config/sample/private_settings.toml" ]; then
     emit_mapping '.config/sample/settings.toml' 'dot_config/sample/private_settings.toml';
   fi
   printf '}\n';;
 *) exit 32;;
esac"#,
        )?;
        let fixture = Self {
            base,
            source,
            config,
            state,
        };
        fixture.enroll()?;
        Ok(fixture)
    }

    fn command(&self) -> Command {
        let mut command = super::common::empty_environment_command();
        command
            .args(["--json", "--home"])
            .arg(&self.base.home)
            .arg("--cache-dir")
            .arg(self.base.file("other-cache"))
            .arg("--settings")
            .arg(self.base.file("settings"))
            .arg("--catalog")
            .arg(&self.base.catalog)
            .env("HOME", self.base.file("caller-home"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.base.home.join("bin").display()),
            );
        command
    }

    fn enroll(&self) -> TestResult {
        let output = self
            .command()
            .arg("--chezmoi-config")
            .arg(&self.config)
            .arg("--chezmoi-source")
            .arg(&self.source)
            .arg("--chezmoi-destination")
            .arg(&self.base.home)
            .args(["config", "enroll"])
            .output()?;
        assert_code(&output, 0);
        assert_code(&self.run(&["config", "list", "--refresh"])?.0, 0);
        Ok(())
    }

    fn run(&self, args: &[&str]) -> Result<(Output, Value), Box<dyn std::error::Error>> {
        let out = self.command().args(args).output()?;
        let body = serde_json::from_slice(&out.stdout)
            .map_err(|error| format!("{error}: {}", String::from_utf8_lossy(&out.stderr)))?;
        Ok((out, body))
    }

    fn target(&self) -> PathBuf {
        self.base.home.join(".gitconfig")
    }
}

#[test]
fn config_adopt_stages_native_mapping_and_publishes_one_source() -> TestResult {
    let f = SourceFixture::new()?;
    let target = f.target();
    let path = target.to_str().ok_or("target path")?;
    let original = b"[fixture]
value=1
";
    fs::write(&target, original)?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644))?;
    let calls = f.base.home.join("native-calls");
    let baseline_calls = fs::read(&calls)?;
    let cache = f.state.join("chezmoi-metadata.json");
    let before_cache = fs::read(&cache)?;
    let (out, data) = f.run(&["--dry-run", "config", "adopt", path])?;
    assert_code(&out, 0);
    assert_eq!(data["data"]["persistentWrites"], false);
    assert_eq!(fs::read(&cache)?, before_cache);
    assert_eq!(fs::read(&calls)?, baseline_calls);
    assert!(!f.source.join("dot_gitconfig").exists());

    let (out, data) = f.run(&["config", "adopt", path, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(data["data"]["phase"], "succeeded");
    assert_eq!(fs::read(f.source.join("dot_gitconfig"))?, original);
    assert_eq!(fs::read(&target)?, original);
    let (out, listing) = f.run(&["config", "list"])?;
    assert_code(&out, 0);
    assert!(
        listing["data"]["candidates"]["shown"]
            .as_array()
            .ok_or("list")?
            .iter()
            .any(|entry| entry == path)
    );
    assert_eq!(fs::read(&calls)?, baseline_calls);
    Ok(())
}

#[test]
fn config_adopt_never_probes_hooks_or_an_alternate_state() -> TestResult {
    let f = SourceFixture::new()?;
    let target = f.target();
    let path = target.to_str().ok_or("target path")?;
    fs::write(
        &target,
        "[fixture]
value=1
",
    )?;
    let calls = f.base.home.join("native-calls");
    let original = fs::read(&calls)?;
    let alternate = f.base.file("alternate-state");
    let (out, data) = f.run(&[
        "--state-dir",
        alternate.to_str().ok_or("state")?,
        "--dry-run",
        "config",
        "adopt",
        path,
    ])?;
    assert_code(&out, 3);
    assert_eq!(data["error"]["code"], "chezmoi_source_state_unsupported");
    assert!(!alternate.exists());

    fs::write(
        &f.config,
        "[hooks.dump-config.pre]
command='/usr/bin/true'
",
    )?;
    for args in [
        vec!["--dry-run", "config", "adopt", path],
        vec!["config", "adopt", path, "--yes"],
    ] {
        let (out, _) = f.run(&args)?;
        assert_code(&out, 3);
        assert_eq!(fs::read(&calls)?, original);
        assert!(!f.source.join("dot_gitconfig").exists());
    }
    Ok(())
}

#[test]
fn config_adopt_rejects_second_paths_to_an_already_managed_file() -> TestResult {
    let f = SourceFixture::new()?;
    let target = f.target();
    fs::write(&target, "[fixture]\nvalue=1\n")?;
    let path = target.to_str().ok_or("target")?;
    assert_code(&f.run(&["config", "adopt", path, "--yes"])?.0, 0);
    let cache = f.state.join("chezmoi-metadata.json");
    let original = fs::read(&cache)?;
    let alias = f.base.home.join(".gitconfig-alias");
    fs::hard_link(&target, &alias)?;
    for candidate in [target, alias] {
        let (out, data) = f.run(&[
            "config",
            "adopt",
            candidate.to_str().ok_or("target")?,
            "--yes",
        ])?;
        assert_code(&out, 3);
        assert_eq!(data["error"]["code"], "chezmoi_target_managed");
        assert_eq!(fs::read(&cache)?, original);
    }
    Ok(())
}

#[test]
fn config_adopt_does_not_allow_paths_that_bypass_the_physical_home_guard() -> TestResult {
    let f = SourceFixture::new()?;
    let fake_home = f.base.home.join("nested/home");
    fs::create_dir_all(&fake_home)?;
    let target = fake_home.join("test");
    fs::write(&target, "fixture")?;
    let mut command = super::common::empty_environment_command();
    let out = command
        .args(["--json", "--home"])
        .arg(&fake_home)
        .env("HOME", &fake_home)
        .env("TMPDIR", fake_home.parent().ok_or("parent")?)
        .args(["--dry-run", "config", "adopt"])
        .arg(&target)
        .output()?;
    let data: Value = serde_json::from_slice(&out.stdout)?;
    assert_code(&out, 3);
    assert_eq!(data["error"]["code"], "chezmoi_source_state_unsupported");
    assert!(!fake_home.join(".local").exists());

    fs::set_permissions(f.base.root.path(), fs::Permissions::from_mode(0o755))?;
    let (out, data) = f.run(&[
        "--dry-run",
        "config",
        "adopt",
        f.target().to_str().ok_or("target")?,
    ])?;
    assert_code(&out, 3);
    assert_eq!(data["error"]["code"], "chezmoi_source_state_unsupported");
    Ok(())
}

#[test]
fn config_adopt_maps_existing_nested_directories_and_preserves_native_neighbors() -> TestResult {
    let f = SourceFixture::new()?;
    fs::create_dir(f.source.join(".git"))?;
    fs::write(
        f.source.join(".git/untouched"),
        "fixture repository artifact",
    )?;
    let parent = f.source.join("dot_config/sample");
    fs::create_dir_all(&parent)?;
    assert_code(&f.run(&["config", "list", "--refresh"])?.0, 0);
    let target = f.base.home.join(".config/sample/settings.toml");
    fs::create_dir_all(target.parent().ok_or("parent")?)?;
    fs::write(&target, "fixture private configuration")?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
    let (out, operation) =
        f.run(&["config", "adopt", target.to_str().ok_or("target")?, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(
        fs::read(parent.join("private_settings.toml"))?,
        fs::read(&target)?
    );
    let op = operation["data"]["id"].as_str().ok_or("operation")?;
    let calls = fs::read(f.base.home.join("native-calls"))?;
    let (out, recovered) = f.run(&["recover", op])?;
    assert_code(&out, 0);
    assert_eq!(recovered["data"]["phase"], "succeeded");
    assert_eq!(fs::read(f.base.home.join("native-calls"))?, calls);
    assert_eq!(
        fs::read(f.source.join(".git/untouched"))?,
        b"fixture repository artifact"
    );
    Ok(())
}

#[test]
fn config_adopt_keeps_live_source_when_stage_or_confirmation_fails() -> TestResult {
    let f = SourceFixture::new()?;
    let original_cache = fs::read(f.state.join("chezmoi-metadata.json"))?;
    let bad = f.base.home.join("native-refuses");
    fs::write(&bad, "fixture")?;
    let (out, result) = f.run(&["config", "adopt", bad.to_str().ok_or("target")?, "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(result["error"]["code"], "chezmoi_source_native_unknown");
    assert!(!f.state.join("active.json").exists());
    let target = f.target();
    fs::write(&target, "fixture")?;
    let (out, result) = f.run(&["config", "adopt", target.to_str().ok_or("target")?])?;
    assert_code(&out, 3);
    assert_eq!(result["error"]["code"], "confirmation_required");
    assert!(!f.state.join("active.json").exists());
    assert!(!f.source.join("dot_gitconfig").exists());
    assert_eq!(
        fs::read(f.state.join("chezmoi-metadata.json"))?,
        original_cache
    );
    assert!(
        !fs::read_dir(&f.state)?
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".chezmoi-source-"))
    );
    Ok(())
}

#[test]
fn config_adopt_protects_state_and_config_through_physical_aliases() -> TestResult {
    let f = SourceFixture::new()?;
    let live_config = f.config.clone();
    let original = fs::read(&live_config)?;
    let logfile = f.base.home.join("native-calls");
    let calls = fs::read(&logfile)?;
    let original_state = f.state.join("chezmoi-owner.json");
    let mut paths = vec![live_config.clone(), original_state];
    let hardlink = f.base.home.join("config-hardlink");
    fs::hard_link(&live_config, &hardlink)?;
    paths.push(hardlink);
    let case_alias = f.base.home.join(".LOCAL/state/macops/chezmoi-owner.json");
    if case_alias.exists() {
        paths.push(case_alias);
    }
    for path in paths {
        for dry in [true, false] {
            let mut args = Vec::new();
            if dry {
                args.push("--dry-run");
            }
            args.extend(["config", "adopt", path.to_str().ok_or("path")?, "--yes"]);
            let (out, data) = f.run(&args)?;
            assert_code(&out, 3);
            assert_eq!(data["error"]["code"], "chezmoi_target_protected");
        }
    }
    assert_eq!(fs::read(live_config)?, original);
    assert_eq!(fs::read(logfile)?, calls);
    assert_eq!(fs::read_dir(f.source)?.count(), 0);
    assert!(!f.state.join("active.json").exists());
    Ok(())
}

#[test]
fn config_forget_retains_the_real_configuration_and_clears_the_native_mapping() -> TestResult {
    let f = SourceFixture::new()?;
    let target = f.target();
    let path = target.to_str().ok_or("target")?;
    fs::write(&target, b"fixture configuration")?;
    let (adopted, journal) = f.run(&["config", "adopt", path, "--yes"])?;
    assert_code(&adopted, 0);
    let id = journal["data"]["plan"]["package"]["id"]
        .as_str()
        .ok_or("config identity")?;
    let source = f.source.join("dot_gitconfig");
    let original_source = fs::read(&source)?;
    fs::write(&target, b"changed local configuration")?;
    let target_meta = fs::metadata(&target)?;
    let cache = f.state.join("chezmoi-metadata.json");
    let before = fs::read(&cache)?;
    let calls = fs::read(f.base.home.join("native-calls"))?;

    let (out, preview) = f.run(&["--dry-run", "config", "forget", path])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["persistentWrites"], false);
    assert_eq!(preview["data"]["nativeInvocations"], 0);
    assert_eq!(fs::read(&source)?, original_source);
    assert_eq!(fs::read(&cache)?, before);
    let (out, result) = f.run(&["config", "forget", id, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(result["data"]["phase"], "succeeded");
    assert!(!source.exists());
    let (_, listing) = f.run(&["config", "list"])?;
    let targets = listing["data"]["candidates"]["shown"]
        .as_array()
        .ok_or("targets")?;
    assert!(!targets.iter().any(|entry| entry == path));
    assert_eq!(fs::read(&target)?, b"changed local configuration");
    assert_eq!(fs::metadata(target)?.ino(), target_meta.ino());
    assert_eq!(fs::read(f.base.home.join("native-calls"))?, calls);
    assert!(
        !fs::read_dir(&f.state)?
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".chezmoi-source-"))
    );
    Ok(())
}

#[test]
fn config_adopt_keeps_live_source_when_native_results_do_not_prove_the_selected_file() -> TestResult
{
    for (label, stage_only, code) in [
        (
            "verification failed",
            r#"if [ "$action" = verify ]; then exit 14; fi"#,
            "chezmoi_source_native_unknown",
        ),
        (
            "mapping missing",
            r#"if [ "$action" = managed ]; then printf '{}\n';exit 0;fi"#,
            "chezmoi_source_unsupported",
        ),
        (
            "mapping unexpected",
            r#"if [ "$action" = verify ]; then exit 0; fi
if [ "$action" = managed ]; then
  printf '{".gitconfig":{"absolute":"%s/.gitconfig","sourceAbsolute":"%s/other/dot_gitconfig","sourceRelative":"other/dot_gitconfig"}}\n' "$root" "$src"
  exit 0
fi"#,
            "chezmoi_source_unsupported",
        ),
        (
            "staged file different",
            r#"if [ "$action" = verify ]; then printf 'unexpected' > "$src/dot_gitconfig";exit 0;fi"#,
            "chezmoi_source_unsupported",
        ),
    ] {
        let f = SourceFixture::new()?;
        let binary = f.base.home.join("bin/chezmoi");
        let original = fs::read_to_string(&binary)?;
        let insertion = format!(
            "if [ \"$HOME\" != \"$MACOPS_TEST_HOME\" ]; then\n{stage_only}\nfi\ncase \"$action\" in"
        );
        fs::write(
            &binary,
            original.replacen("case \"$action\" in", &insertion, 1),
        )?;
        f.enroll()?;
        let target = f.target();
        fs::write(&target, "fixture")?;
        let cache = f.state.join("chezmoi-metadata.json");
        let before = fs::read(&cache)?;
        let calls = fs::read(f.base.home.join("native-calls"))?;
        let (out, result) =
            f.run(&["config", "adopt", target.to_str().ok_or("target")?, "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(result["error"]["code"], code, "{label}");
        assert_eq!(fs::read(&cache)?, before);
        assert!(!f.state.join("active.json").exists());
        assert_eq!(fs::read(f.base.home.join("native-calls"))?, calls);
        assert!(!f.source.join("dot_gitconfig").exists());
    }
    Ok(())
}

#[test]
fn config_adopt_does_not_stage_a_configuration_with_an_independent_hard_link() -> TestResult {
    let f = SourceFixture::new()?;
    let target = f.target();
    fs::write(&target, "fixture")?;
    fs::hard_link(&target, f.base.home.join("unregistered-link"))?;
    let binary = f.base.home.join("bin/chezmoi");
    let checked = fs::read_to_string(&binary)?.replacen(
        r#"case "$action" in"#,
        r#"if [ "$HOME" != "$MACOPS_TEST_HOME" ] && [ "$action" = add ]; then exit 19; fi
case "$action" in"#,
        1,
    );
    fs::write(binary, checked)?;
    f.enroll()?;
    let before = fs::read(f.base.home.join("native-calls"))?;
    let (out, result) = f.run(&["config", "adopt", target.to_str().ok_or("target")?, "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(result["error"]["code"], "chezmoi_source_unsupported");
    assert_eq!(fs::read(f.base.home.join("native-calls"))?, before);
    assert!(!f.state.join("active.json").exists());
    assert!(!f.source.join("dot_gitconfig").exists());
    Ok(())
}

#[test]
fn config_forget_requires_an_existing_mapping_and_verified_native_removal() -> TestResult {
    for (mock_forget, code) in [
        ("exit 21", "chezmoi_source_native_unknown"),
        ("exit 0", "chezmoi_source_unsupported"),
    ] {
        let f = SourceFixture::new()?;
        let binary = f.base.home.join("bin/chezmoi");
        let original = fs::read_to_string(&binary)?;
        let insertion = format!(
            r#"if [ "$action" = forget ] && [ "$HOME" != "$MACOPS_TEST_HOME" ]; then {mock_forget}; fi
case "$action" in"#
        );
        fs::write(
            &binary,
            original.replacen(r#"case "$action" in"#, &insertion, 1),
        )?;
        f.enroll()?;
        let target = f.target();
        fs::write(&target, "fixture")?;
        let path = target.to_str().ok_or("target")?;
        for missing in [path, "config:missing"] {
            let (out, data) = f.run(&["config", "forget", missing, "--yes"])?;
            assert_code(&out, 3);
            assert_eq!(data["error"]["code"], "chezmoi_target_unmanaged");
        }
        assert_code(&f.run(&["config", "adopt", path, "--yes"])?.0, 0);
        let source = f.source.join("dot_gitconfig");
        let cache = f.state.join("chezmoi-metadata.json");
        let active = f.state.join("active.json");
        let before = fs::read(&cache)?;
        let before_record = fs::read(&active)?;
        let (out, result) = f.run(&["config", "forget", path, "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(result["error"]["code"], code);
        assert_eq!(fs::read(source)?, b"fixture");
        assert_eq!(fs::read(target)?, b"fixture");
        assert_eq!(fs::read(cache)?, before);
        assert_eq!(fs::read(active)?, before_record);
    }
    Ok(())
}

#[test]
fn config_forget_requires_the_exact_mapping_before_and_after_native_execution() -> TestResult {
    for native_fixture in [
        r#"if [ "$action" = managed ] && [ -f "$src/dot_gitconfig" ] && [ "$(/bin/cat "$src/dot_gitconfig")" = 'static ordinary file' ]; then
  printf '{".gitconfig":{"absolute":"%s/.gitconfig","sourceAbsolute":"%s/dot_other","sourceRelative":"dot_other"}}\n' "$root" "$src"
  exit 0
fi"#,
        r#"if [ "$action" = forget ]; then /bin/rm "$src/dot_gitconfig"; /usr/bin/touch "$src/extra-marker";exit 0;fi
if [ "$action" = managed ] && [ -f "$src/extra-marker" ]; then
  printf '{".gitconfig":{"absolute":"%s/.gitconfig","sourceAbsolute":"%s/dot_gitconfig","sourceRelative":"dot_gitconfig"}}\n' "$root" "$src"
  exit 0
fi"#,
    ] {
        let f = SourceFixture::new()?;
        let binary = f.base.home.join("bin/chezmoi");
        let original = fs::read_to_string(&binary)?;
        let insertion = format!(
            r#"if [ "$HOME" != "$MACOPS_TEST_HOME" ]; then
{native_fixture}
fi
case "$action" in"#
        );
        fs::write(
            binary,
            original.replacen(r#"case "$action" in"#, &insertion, 1),
        )?;
        f.enroll()?;
        let target = f.target();
        fs::write(&target, "unchanged destination")?;
        let path = target.to_str().ok_or("target")?;
        assert_code(&f.run(&["config", "adopt", path, "--yes"])?.0, 0);
        let journal = f.state.join("active.json");
        let before = fs::read(&journal)?;
        let cache = f.state.join("chezmoi-metadata.json");
        let original_cache = fs::read(&cache)?;
        let (out, result) = f.run(&["config", "forget", path, "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(result["error"]["code"], "chezmoi_source_unsupported");
        assert_eq!(
            fs::read(f.source.join("dot_gitconfig"))?,
            b"unchanged destination"
        );
        assert_eq!(fs::read(target)?, b"unchanged destination");
        assert_eq!(fs::read(journal)?, before);
        assert_eq!(fs::read(cache)?, original_cache);
    }
    Ok(())
}
