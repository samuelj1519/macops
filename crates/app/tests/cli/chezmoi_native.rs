use super::{Fixture, TestResult, assert_code};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

struct Native {
    fixture: Fixture,
    source: PathBuf,
    destination: PathBuf,
    config: PathBuf,
}
impl Native {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let fixture = Fixture::new()?;
        let source = fixture.home.join("source");
        let destination = fixture.home.join("destination");
        let config = fixture.home.join("chezmoi.toml");
        fs::create_dir_all(source.join("home"))?;
        fs::create_dir_all(&destination)?;
        fs::write(source.join(".chezmoiroot"), "home\n")?;
        fs::write(&config, "[data]\nmessage='fixture'\n")?;
        fs::write(source.join("home/dot_demo.tmpl"), "fixture\n")?;
        fs::write(source.join("home/dot_other"), "other\n")?;
        fixture.script("chezmoi",r#"
src="$HOME/source";dst="$HOME/destination";config="$HOME/chezmoi.toml";operation='';selected=''
while [ "$#" -gt 0 ]; do
 case "$1" in
 --source) src="$2";shift 2;; --destination) dst="$2";shift 2;; --config) config="$2";shift 2;;
 --cache|--persistent-state) shift 2;; --force) exit 99;; --version) operation=version;shift;;
 dump-config|source-path|target-path|execute-template|managed|status|apply|verify) operation="$1";shift;;
 --) shift;selected="$*";break;; *) shift;; esac
done
printf '%s\n' "$operation" >> "$HOME/native-calls"
if [ -f "$HOME/native-output-$operation" ]; then /bin/cat "$HOME/native-output-$operation";exit 0;fi
if [ -f "$HOME/native-error-$operation" ]; then exit 17;fi
if [ -f "$dst/.demo" ] && [ -f "$HOME/native-after-output-$operation" ]; then /bin/cat "$HOME/native-after-output-$operation";exit 0;fi
if [ -f "$HOME/leak" ]; then printf '%s\n' 'SYNTHETIC_API_NEVER_PUBLIC';printf '%s\n' 'SYNTHETIC_API_NEVER_PUBLIC' >&2;exit 17;fi
case "$operation" in
 version) printf 'chezmoi version v2.74.1\n';;
 dump-config) if [ -f "$HOME/filter-files" ];then exclude='["files"]';else exclude='[]';fi
 printf '{"sourceDir":"%s","destDir":"%s","status":{"exclude":[]},"verify":{"exclude":%s}}\n' "$src" "$dst" "$exclude";;
 source-path) if [ -z "$selected" ];then printf '%s/home\n' "$src";else
   case "$selected" in "$dst/.demo") printf '%s/home/dot_demo.tmpl\n' "$src";; "$dst/.other") printf '%s/home/dot_other\n' "$src";; *)exit 12;;esac;fi;;
 target-path)printf '%s\n' "$dst";; execute-template)
 if [ -f "$HOME/new-native-state" ];then configured="$HOME/new-state.boltdb";else configured='';fi
 printf '{"configFile":"%s","cache":"%s/native-cache","configuredState":"%s","resolvedDefault":"%s/chezmoistate.boltdb"}' "$config" "$HOME" "$configured" "$HOME";;
 managed) printf '{".demo":{"absolute":"%s/.demo","sourceAbsolute":"%s/home/dot_demo.tmpl","sourceRelative":"dot_demo.tmpl"},".other":{"absolute":"%s/.other","sourceAbsolute":"%s/home/dot_other","sourceRelative":"dot_other"}}\n' "$dst" "$src" "$dst" "$src";;
 status|verify|apply)
   if [ -z "$selected" ];then selected="$dst/.demo $dst/.other";fi
   for target in $selected;do
    case "$target" in "$dst/.demo") input="$src/home/dot_demo.tmpl";; "$dst/.other")input="$src/home/dot_other";; *)exit 13;;esac
    case "$operation" in
     status) if [ ! -f "$target" ];then printf ' A %s\n' "$target";elif ! /usr/bin/cmp -s "$input" "$target";then printf ' M %s\n' "$target";fi;;
     verify) /usr/bin/cmp -s "$input" "$target"||exit 1;;
     apply) if [ -f "$HOME/conflict" ];then exit 1;fi
      /bin/cp "$input" "$target"
      if [ -f "$HOME/native-fail" ];then exit 17;fi
      if [ -f "$HOME/native-helper" ];then
        (/bin/sleep 2; printf finished > "$HOME/native-helper-done") >/dev/null 2>&1 &
      fi
      if [ -f "$HOME/native-sleep" ];then /bin/sleep 2;fi;;
    esac
   done;; *) exit 91;; esac
"#)?;
        Ok(Self {
            fixture,
            source,
            destination,
            config,
        })
    }
    fn run(
        &self,
        args: &[&str],
    ) -> Result<(std::process::Output, Value), Box<dyn std::error::Error>> {
        self.fixture.run(args)
    }
    fn enroll(&self) -> TestResult {
        let out = self
            .fixture
            .command()
            .arg("--chezmoi-source")
            .arg(&self.source)
            .arg("--chezmoi-destination")
            .arg(&self.destination)
            .arg("--chezmoi-config")
            .arg(&self.config)
            .args(["config", "enroll"])
            .output()?;
        assert_code(&out, 0);
        Ok(())
    }
    fn calls(&self) -> Result<String, std::io::Error> {
        fs::read_to_string(self.fixture.home.join("native-calls"))
    }
}
pub(super) fn run_browser_pty(
    fixture: &Fixture,
    args: &[&str],
    scenario: &str,
    target: &Path,
    width: &str,
    expected: &str,
) -> TestResult {
    let command = fixture.command();
    let mut test = std::process::Command::new("python3");
    test.args(["-B", "-c", include_str!("browser_pty.py"), scenario])
        .arg(target)
        .args([width, expected])
        .arg(command.get_program())
        .args(command.get_args().filter(|value| *value != "--json"))
        .args(args);
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            test.env(key, value);
        }
    }
    let result = test.output()?;
    assert_code(&result, 0);
    Ok(())
}

#[test]
fn config_browser_pty_opens_selected_files_and_refuses_unbound_replacement_and_links() -> TestResult
{
    for (scenario, width) in [
        ("open", "112"),
        ("replaced", "72"),
        ("link", "72"),
        ("large", "72"),
        ("binary", "72"),
        ("root", "72"),
        ("owner", "72"),
        ("long", "72"),
    ] {
        let f = Native::new()?;
        f.enroll()?;
        let target = f.destination.join(".demo");
        let source = f.source.join("home/dot_demo.tmpl");
        fs::write(&source, "synthetic-source-content\n")?;
        match scenario {
            "link" => std::os::unix::fs::symlink(&source, &target)?,
            "large" => fs::write(&target, vec![b'x'; 256 * 1024 + 1])?,
            "binary" => fs::write(&target, b"\0synthetic-binary")?,
            "long" => fs::write(
                &target,
                format!("{}tail-of-long-single-line", "x".repeat(4800)),
            )?,
            _ => fs::write(&target, "synthetic-live-content\n\x1b]52;redacted\x07")?,
        }
        fs::write(f.fixture.home.join("native-output-status"), "")?;
        let (refreshed, _) = f.run(&["config", "list", "--refresh"])?;
        assert_code(&refreshed, 0);
        let calls = f.calls()?;
        run_browser_pty(&f.fixture, &["config"], scenario, &target, width, "0")?;
        assert_eq!(f.calls()?, calls);
    }
    Ok(())
}

#[test]
fn config_browser_pty_keeps_persisted_cache_when_native_status_is_unavailable() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    let (refreshed, _) = f.run(&["config", "list", "--refresh"])?;
    assert_code(&refreshed, 0);
    let calls = f.calls()?;
    let cache = f.fixture.file("state/chezmoi-metadata.json");
    let original = fs::read(&cache)?;
    fs::write(f.fixture.home.join("native-error-status"), "")?;
    run_browser_pty(
        &f.fixture,
        &["config"],
        "refresh",
        &f.destination,
        "72",
        "3",
    )?;
    assert_eq!(fs::read(cache)?, original);
    assert_eq!(f.calls()?.strip_prefix(&calls), Some("managed\nstatus\n"));
    assert!(!f.destination.join(".demo").exists());
    Ok(())
}

#[test]
fn config_browser_r_retries_only_after_a_confirmed_exit_and_publishes_success() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    let original_calls = f.calls()?;
    let marker = f.fixture.home.join("native-error-status");
    fs::write(&marker, "")?;
    run_browser_pty(&f.fixture, &["config"], "refresh-retry", &marker, "72", "0")?;
    assert_eq!(
        f.calls()?.strip_prefix(&original_calls),
        Some("managed\nstatus\nmanaged\nstatus\n")
    );
    let (out, result) = f.run(&["config"])?;
    assert_code(&out, 0);
    assert_eq!(result["data"]["items"].as_array().ok_or("items")?.len(), 2);
    assert!(!f.destination.join(".demo").exists());
    Ok(())
}

#[test]
fn config_browser_uses_verified_empty_instead_of_unavailable_when_no_targets_exist() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    fs::write(f.fixture.home.join("native-output-managed"), "{}")?;
    fs::write(f.fixture.home.join("native-output-status"), "")?;
    let (refreshed, _) = f.run(&["config", "list", "--refresh"])?;
    assert_code(&refreshed, 0);
    let (out, value) = f.run(&["config"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["items"], json!([]));
    assert!(
        value["data"]["note"]
            .as_str()
            .is_some_and(|text| text.contains("当前范围无受管文件"))
    );
    Ok(())
}

#[test]
fn config_browser_never_uses_historical_mapping_as_file_authority() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    fs::write(f.destination.join(".demo"), "synthetic-live-content")?;
    let (out, _) = f.run(&["config", "list", "--refresh"])?;
    assert_code(&out, 0);
    let calls = f.calls()?;
    fs::remove_file(f.fixture.file("state/chezmoi-owner.json"))?;
    let (out, view) = f.run(&["config"])?;
    assert_code(&out, 3);
    assert_eq!(view["data"]["coverage"], "historical");
    assert_eq!(
        view["data"]["items"]
            .as_array()
            .ok_or("historical mapping")?
            .len(),
        2
    );
    run_browser_pty(
        &f.fixture,
        &["config"],
        "archived",
        &f.destination,
        "72",
        "3",
    )?;
    assert_eq!(f.calls()?, calls);
    Ok(())
}

#[test]
fn config_browser_rejects_tool_replacement_before_explicit_refresh_and_retains_cache() -> TestResult
{
    let f = Native::new()?;
    f.enroll()?;
    let (refreshed, _) = f.run(&["config", "list", "--refresh"])?;
    assert_code(&refreshed, 0);
    let calls = f.calls()?;
    let cache = f.fixture.file("state/chezmoi-metadata.json");
    let original = fs::read(&cache)?;
    let tool = f.fixture.home.join("bin/chezmoi");
    run_browser_pty(
        &f.fixture,
        &[
            "--chezmoi-source",
            path_text(&f.source)?,
            "--chezmoi-destination",
            path_text(&f.destination)?,
            "--chezmoi-config",
            path_text(&f.config)?,
            "config",
        ],
        "tool",
        &tool,
        "72",
        "3",
    )?;
    assert_eq!(fs::read(cache)?, original);
    assert_eq!(f.calls()?, calls);
    assert!(!f.fixture.home.join("replacement-executed").exists());
    Ok(())
}

#[test]
fn config_browser_uses_a_directory_filter_and_plain_output_only_lists_files() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    fs::create_dir(f.source.join("home/dot_group-directory"))?;
    fs::create_dir(f.destination.join(".group-directory"))?;
    fs::write(
        f.fixture.home.join("native-output-managed"),
        serde_json::to_vec(&json!({
            ".group-directory":{"absolute":f.destination.join(".group-directory"),
                "sourceAbsolute":f.source.join("home/dot_group-directory"),
                "sourceRelative":"dot_group-directory"},
            ".demo":{"absolute":f.destination.join(".demo"),
                "sourceAbsolute":f.source.join("home/dot_demo.tmpl"),"sourceRelative":"dot_demo.tmpl"}
        }))?,
    )?;
    fs::write(f.fixture.home.join("native-output-status"), "")?;
    let (refresh, _) = f.run(&["config", "list", "--refresh"])?;
    assert_code(&refresh, 0);
    run_browser_pty(
        &f.fixture,
        &["config"],
        "directory",
        &f.destination,
        "72",
        "0",
    )?;
    let fixed = f.fixture.command();
    let (program, arguments) = (
        fixed.get_program(),
        fixed.get_args().filter(|arg| *arg != "--json"),
    );
    let output = std::process::Command::new(program)
        .args(arguments)
        .args(["--dry-run", "config"])
        .output()?;
    assert_code(&output, 0);
    let stdout = std::str::from_utf8(&output.stdout)?;
    assert!(stdout.contains(".demo"));
    assert!(!stdout.contains(".group-directory"));
    assert!(!stdout.contains('\x1b'));
    Ok(())
}

fn path_text(path: &Path) -> Result<&str, Box<dyn std::error::Error>> {
    path.to_str().ok_or_else(|| "utf8".into())
}

#[test]
fn browser_distinguishes_pending_and_complete_inventory_without_the_legacy_summary_limit()
-> TestResult {
    let f = Native::new()?;
    let (before, value) = f.run(&["config"])?;
    assert_code(&before, 3);
    assert_eq!(value["data"]["coverage"], "unregistered");
    assert!(!f.fixture.file("state").exists());
    f.enroll()?;
    let enrollment_calls = f.calls()?;
    let (pending, value) = f.run(&["config"])?;
    assert_code(&pending, 3);
    assert_eq!(value["data"]["coverage"], "pending");
    assert_eq!(f.calls()?, enrollment_calls);
    let mut rows = serde_json::Map::new();
    for index in 0..81 {
        let target = format!("target-{index:02}");
        let source = format!("dot_{target}");
        fs::write(f.source.join("home").join(&source), b"fixture")?;
        rows.insert(
            target.clone(),
            json!({"absolute":f.destination.join(target),"sourceAbsolute": f.source.join("home").join(&source),"sourceRelative":source}),
        );
    }
    fs::write(
        f.fixture.home.join("native-output-managed"),
        serde_json::to_vec(&rows)?,
    )?;
    fs::write(f.fixture.home.join("native-output-status"), "")?;
    let (refresh, old) = f.run(&["config", "list", "--refresh"])?;
    assert_code(&refresh, 0);
    assert_eq!(
        old["data"]["candidates"]["shown"]
            .as_array()
            .ok_or("summary")?
            .len(),
        80
    );
    let queried = f.calls()?;
    let metadata = fs::read(f.fixture.file("state/chezmoi-metadata.json"))?;
    let (browsed, value) = f.run(&["config"])?;
    assert_code(&browsed, 0);
    assert_eq!(value["data"]["items"].as_array().ok_or("items")?.len(), 81);
    assert!(
        value["data"]["items"]
            .as_array()
            .ok_or("items")?
            .iter()
            .any(|row| row["path"] == json!(f.destination.join("target-80"))
                && row["source"] == json!(f.source.join("home/dot_target-80")))
    );
    assert_eq!(f.calls()?, queried);
    assert_eq!(
        fs::read(f.fixture.file("state/chezmoi-metadata.json"))?,
        metadata
    );
    assert!(!f.destination.join("target-80").exists());
    Ok(())
}

#[test]
fn native_enrollment_stops_at_unconfirmed_discovery_and_publishes_no_binding() -> TestResult {
    let discovery = [
        "version",
        "dump-config",
        "source-path",
        "target-path",
        "execute-template",
    ];
    for (index, operation) in discovery.into_iter().enumerate() {
        let f = Native::new()?;
        fs::write(f.fixture.home.join(format!("native-error-{operation}")), "")?;
        let (out, value) = f.run(&["config", "enroll"])?;
        assert_code(&out, 3);
        assert_eq!(value["error"]["code"], "chezmoi_native_query_failed");
        assert_eq!(f.calls()?.lines().collect::<Vec<_>>(), discovery[..=index]);
        for file in ["chezmoi-owner.json", "chezmoi-metadata.json", "active.json"] {
            assert!(!f.fixture.file("state").join(file).exists());
        }
    }
    Ok(())
}

#[test]
fn native_enrollment_rejects_malformed_success_before_publishing_scope() -> TestResult {
    for (operation, bytes) in [
        ("version", b"v2.74.1\n".as_slice()),
        ("version", b"chezmoi version vbroken\n".as_slice()),
        ("dump-config", b"{}".as_slice()),
        ("dump-config", b"{\"sourceDir\":\"relative\"}".as_slice()),
        ("source-path", b"/first\n/second\n".as_slice()),
        ("target-path", b"relative\n".as_slice()),
        ("execute-template", b"{\"configFile\":\"/cfg\"}".as_slice()),
    ] {
        let f = Native::new()?;
        fs::write(
            f.fixture.home.join(format!("native-output-{operation}")),
            bytes,
        )?;
        let (out, value) = f.run(&["config", "enroll"])?;
        assert_code(&out, 3);
        assert_eq!(f.calls()?.lines().last(), Some(operation), "{value}");
        assert!(!f.fixture.file("state/chezmoi-owner.json").exists());
        assert!(!f.fixture.file("state/chezmoi-metadata.json").exists());
    }
    Ok(())
}

#[test]
fn native_refresh_or_selected_query_rejects_incomplete_scope_without_losing_cache() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    let (out, _) = f.run(&["config", "list", "--refresh"])?;
    assert_code(&out, 0);
    let cache = f.fixture.file("state/chezmoi-metadata.json");
    let original = fs::read(&cache)?;
    let target = f.destination.join(".demo");
    for (operation, args) in [
        ("managed", vec!["config", "list", "--refresh"]),
        ("status", vec!["config", "list", "--refresh"]),
        ("source-path", vec!["config", "diff", path_text(&target)?]),
        ("status", vec!["config", "diff", path_text(&target)?]),
    ] {
        let marker = f.fixture.home.join(format!("native-error-{operation}"));
        fs::write(&marker, "")?;
        let (out, value) = f.run(&args)?;
        assert_code(&out, 3);
        assert_eq!(value["error"]["code"], "chezmoi_native_query_failed");
        assert_eq!(original, fs::read(&cache)?);
        fs::remove_file(marker)?;
    }
    fs::write(f.fixture.home.join("native-output-source-path"), "/tmp\n")?;
    let (out, _) = f.run(&["config", "diff", path_text(&target)?])?;
    assert_code(&out, 3);
    assert_eq!(original, fs::read(&cache)?);
    assert!(f.calls()?.lines().all(|op| op != "apply"));
    assert!(!target.exists());
    Ok(())
}

#[test]
fn native_apply_exit_zero_requires_an_authoritative_post_observation() -> TestResult {
    for (marker, contents) in [
        ("native-error-verify", ""),
        ("native-after-output-status", "not valid status\n"),
    ] {
        let f = Native::new()?;
        f.enroll()?;
        let target = f.destination.join(".demo");
        fs::write(f.fixture.home.join(marker), contents)?;
        let (out, value) = f.run(&["config", "apply", path_text(&target)?, "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(value["data"]["phase"], "unknown");
        assert_eq!(value["data"]["nativeOutcome"]["code"], 0);
        assert_ne!(value["data"]["filesMatch"], true);
        assert_eq!(fs::read_to_string(&target)?, "fixture\n");
        let id = value["data"]["operation_id"].as_str().ok_or("operation")?;
        let (out, replay) = f.run(&["recover", id])?;
        assert_code(&out, 3);
        assert_eq!(replay["data"]["phase"], "unknown");
        assert_eq!(f.calls()?.lines().filter(|op| *op == "apply").count(), 1);
    }
    Ok(())
}

#[test]
fn native_timeout_never_reissues_apply_when_the_original_helper_lingers() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    fs::write(f.fixture.home.join("native-sleep"), "")?;
    let target = f.destination.join(".demo");
    let (out, value) = f.run(&[
        "--timeout-secs",
        "1",
        "config",
        "apply",
        path_text(&target)?,
        "--yes",
    ])?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["phase"], "unknown");
    let id = value["data"]["operation_id"].as_str().ok_or("operation")?;
    let before = f.calls()?;
    assert_eq!(before.lines().filter(|op| *op == "apply").count(), 1);
    let (out, replay) = f.run(&["recover", id])?;
    assert_code(&out, 3);
    assert_eq!(replay["data"]["phase"], "unknown");
    assert_eq!(f.calls()?.lines().filter(|op| *op == "apply").count(), 1);
    Ok(())
}

#[test]
fn native_apply_waits_for_a_helper_that_outlives_its_direct_child() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    fs::write(f.fixture.home.join("native-helper"), "")?;
    let target = f.destination.join(".demo");
    let (out, value) = f.run(&["config", "apply", path_text(&target)?, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["phase"], "succeeded");
    assert_eq!(
        fs::read_to_string(f.fixture.home.join("native-helper-done"))?,
        "finished"
    );
    assert_eq!(f.calls()?.lines().filter(|op| *op == "apply").count(), 1);
    Ok(())
}

#[test]
fn native_unenrolled_and_dryrun_and_cached_queries_are_zero_invocations() -> TestResult {
    let f = Native::new()?;
    let target = f.destination.join(".demo");
    for args in [
        vec!["config", "list"],
        vec!["config", "where", "demo"],
        vec!["--dry-run", "config", "enroll"],
        vec!["--dry-run", "config", "apply", path_text(&target)?, "--yes"],
        vec!["--dry-run", "config", "list", "--refresh"],
        vec!["--dry-run", "config", "verify", path_text(&target)?],
    ] {
        let (_, value) = f.run(&args)?;
        assert_eq!(value["data"]["nativeInvocations"], 0);
    }
    let (out, value) = f.run(&["config", "diff", path_text(&target)?])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "chezmoi_binding_unresolved");
    assert!(!f.fixture.home.join("native-calls").exists());
    assert!(!f.fixture.file("state").exists());
    Ok(())
}

#[test]
fn native_enroll_selected_apply_and_complete_range_queries_keep_scope() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    let target = f.destination.join(".demo");
    let other = f.destination.join(".other");
    let before = f.calls()?;
    let (out, value) = f.run(&["config", "list"])?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["coverage"], "cached");
    assert_eq!(f.calls()?, before);
    let (out, value) = f.run(&["config", "source-path"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["path"], json!(f.source.join("home")));
    let (out, value) = f.run(&["config", "diff", path_text(&target)?])?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["scope"], "selected");
    assert_eq!(value["data"]["results"][0]["path"], json!(target));
    let (out, value) = f.run(&["config", "apply", path_text(&target)?, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["phase"], "succeeded");
    assert_eq!(fs::read_to_string(&target)?, "fixture\n");
    assert!(!other.exists());
    let (out, value) = f.run(&["config", "verify", path_text(&target)?])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["matched"], true);
    let (out, value) = f.run(&["config", "status", path_text(&target)?, path_text(&other)?])?;
    assert_code(&out, 3);
    assert_eq!(
        value["data"]["targets"].as_array().ok_or("targets")?.len(),
        2
    );
    let (out, value) = f.run(&["config", "list", "--refresh"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["candidates"]["count"], 2);
    let before = f.calls()?;
    let (out, value) = f.run(&["config", "where", "demo"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["matches"]["count"], 1);
    assert_eq!(before, f.calls()?);
    let (_, value) = f.run(&["--dry-run", "config", "apply", path_text(&target)?])?;
    assert_eq!(value["data"]["nativeInvocations"], 0);
    let (out, value) = f.run(&["adopt", path_text(&target)?, "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(
        value["error"]["code"],
        "config_native_explicit_entry_required"
    );
    Ok(())
}

#[test]
fn native_apply_resolves_only_the_selected_cached_config_id() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    let calls = f.calls()?;
    let (out, value) = f.run(&["config", "apply", "config:missing", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "not_found");
    assert_eq!(calls, f.calls()?);

    let (out, _) = f.run(&["config", "list", "--refresh"])?;
    assert_code(&out, 0);
    let (_, value) = f.run(&["list", "--kind", "configs"])?;
    let target = f.destination.join(".demo");
    let id = value["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|row| row["target"] == json!(target))
        .and_then(|row| row["id"].as_str())
        .ok_or("observed configuration ID")?;
    assert!(id.starts_with("config:"));
    let (out, value) = f.run(&["config", "apply", id, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["phase"], "succeeded");
    assert_eq!(fs::read_to_string(target)?, "fixture\n");
    assert!(!f.destination.join(".other").exists());
    assert_eq!(
        f.calls()?.lines().filter(|line| *line == "apply").count(),
        1
    );
    Ok(())
}

#[test]
fn native_partial_failure_files_match_is_not_success_and_recovery_never_reapplies() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    fs::write(f.fixture.home.join("native-fail"), "")?;
    let target = f.destination.join(".demo");
    let (out, value) = f.run(&["config", "apply", path_text(&target)?, "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["phase"], "unknown");
    assert_eq!(value["data"]["nativeResult"], "failed-partial-possible");
    assert_eq!(value["data"]["filesMatch"], true);
    let id = value["data"]["operation_id"].as_str().ok_or("id")?;
    let before = f.calls()?.lines().filter(|l| *l == "apply").count();
    fs::remove_file(f.fixture.home.join("native-fail"))?;
    let (out, recovered) = f.run(&["recover", id])?;
    assert_code(&out, 3);
    assert_eq!(recovered["data"]["phase"], "unknown");
    assert_eq!(before, f.calls()?.lines().filter(|l| *l == "apply").count());
    let (out, resolved) = f.run(&["recover", id, "--resolve-failed", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(resolved["data"]["phase"], "failed");
    Ok(())
}

#[test]
fn native_errors_and_receipts_never_retain_secret_output_and_yes_is_not_force() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    let target = f.destination.join(".demo");
    fs::write(&target, "foreign\n")?;
    fs::write(f.fixture.home.join("conflict"), "")?;
    let (out, value) = f.run(&["config", "apply", path_text(&target)?, "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["phase"], "unknown");
    assert_eq!(fs::read_to_string(&target)?, "foreign\n");
    let id = value["data"]["operation_id"].as_str().ok_or("id")?;
    f.run(&["recover", id, "--resolve-failed", "--yes"])?;
    fs::write(f.fixture.home.join("leak"), "")?;
    let (out, value) = f.run(&["config", "diff", path_text(&target)?])?;
    assert_code(&out, 3);
    assert!(!value.to_string().contains("SYNTHETIC_API_NEVER_PUBLIC"));
    for entry in fs::read_dir(f.fixture.file("state"))? {
        let path = entry?.path();
        if path.is_file() {
            assert!(
                !String::from_utf8_lossy(&fs::read(path)?).contains("SYNTHETIC_API_NEVER_PUBLIC")
            );
        }
    }
    Ok(())
}

#[test]
fn native_owner_tampering_and_unmanaged_targets_never_fall_back() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    let unknown = f.destination.join(".unknown");
    let (out, _) = f.run(&["config", "apply", path_text(&unknown)?, "--yes"])?;
    assert_code(&out, 3);
    assert!(!unknown.exists());
    let mut owner = f.fixture.read_json("state/chezmoi-owner.json")?;
    owner["anchors"] = json!([]);
    fs::write(
        f.fixture.file("state/chezmoi-owner.json"),
        serde_json::to_vec(&owner)?,
    )?;
    let (out, value) = f.run(&["config", "source-path"])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "invalid_chezmoi_native");
    Ok(())
}

#[test]
fn native_missing_completion_cannot_be_replaced_by_matching_files_or_restore() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    let target = f.destination.join(".demo");
    let (out, value) = f.run(&["config", "apply", path_text(&target)?, "--yes"])?;
    assert_code(&out, 0);
    let id = value["data"]["operation_id"].as_str().ok_or("id")?;
    let binding = f
        .fixture
        .read_json(&format!("state/native-{id}-binding.json"))?;
    let completion = binding["completion_name"].as_str().ok_or("completion")?;
    fs::remove_file(f.fixture.file("state").join(completion))?;
    let mut journal = f.fixture.read_json("state/active.json")?;
    journal["phase"] = json!("unknown");
    for path in [
        f.fixture.file("state/active.json"),
        f.fixture.file(&format!("state/operations/{id}.json")),
    ] {
        fs::write(path, serde_json::to_vec(&journal)?)?;
    }
    let before = f.calls()?.lines().filter(|l| *l == "apply").count();
    let (out, value) = f.run(&["recover", id])?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["phase"], "unknown");
    assert_eq!(value["data"]["filesMatch"], true);
    assert_eq!(f.calls()?.lines().filter(|l| *l == "apply").count(), before);
    let (out, value) = f.run(&["config", "restore", id])?;
    assert_code(&out, 3);
    assert_eq!(
        value["error"]["code"],
        "native_restore_requires_forward_operation"
    );
    Ok(())
}

#[test]
fn native_cached_resource_metadata_and_owner_identity_are_never_live_queries() -> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    f.run(&["config", "list", "--refresh"])?;
    let before = f.calls()?;
    let (_, value) = f.run(&["list", "--kind", "configs"])?;
    assert_eq!(before, f.calls()?);
    assert!(
        value["data"]["resources"]
            .as_array()
            .ok_or("resources")?
            .iter()
            .any(|r| r["owner"] == "chezmoi-native-cached")
    );
    let other = f.fixture.home.join("different-source");
    fs::create_dir(&other)?;
    let out = f
        .fixture
        .command()
        .arg("--chezmoi-source")
        .arg(other)
        .args(["config", "source-path"])
        .output()?;
    assert_code(&out, 3);
    assert_eq!(before, f.calls()?);
    fs::rename(&f.source, f.fixture.home.join("old-source"))?;
    fs::create_dir_all(f.source.join("home"))?;
    let (out, _) = f.run(&["config", "source-path"])?;
    assert_code(&out, 3);
    assert_eq!(before, f.calls()?);
    Ok(())
}

#[test]
fn native_reported_configuration_without_a_source_leaf_stays_visible_from_cache() -> TestResult {
    let f = Native::new()?;
    let source = f.source.join("home/dot_demo.tmpl");
    let target = f.destination.join(".demo");
    fs::remove_file(&source)?;
    fs::write(&target, b"existing native file")?;
    f.enroll()?;
    let (out, refreshed) = f.run(&["config", "list", "--refresh"])?;
    assert_code(&out, 0);
    assert_eq!(refreshed["data"]["candidates"]["count"], 2);
    let cached_calls = f.calls()?;
    let (_, value) = f.run(&["list", "--kind", "configs"])?;
    let rows = value["data"]["resources"]
        .as_array()
        .ok_or("config resources")?;
    let row = rows
        .iter()
        .find(|row| row["target"] == json!(target))
        .ok_or("virtual config")?;
    assert_eq!(row["owner"], "chezmoi-native-virtual");
    assert_eq!(row["status"], "present");
    assert_eq!(row["source_candidates"], json!([]));
    assert_eq!(f.calls()?, cached_calls);
    assert_eq!(fs::read(&target)?, b"existing native file");
    assert!(!source.exists());
    Ok(())
}

#[test]
fn legacy_readonly_config_receipt_can_refresh_its_baseline_without_native_authority() -> TestResult
{
    let f = Fixture::new()?;
    let target = f.home.join(".zshrc");
    fs::write(&target, "owned-local\n")?;
    f.script(
        "chezmoi",
        "printf called > \"$MACOPS_TEST_HOME/native-called\";exit 91",
    )?;
    let (out, value) = f.run(&["list", "--kind", "configs"])?;
    assert_code(&out, 0);
    let id = value["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|row| row["target"] == json!(target))
        .and_then(|row| row["id"].as_str())
        .ok_or("config id")?;
    let package = json!({"id":id,"name":target,"kind":"config","official_url":"","provider":{"type":"config-observed","target":target,"authority":"untracked"},"paths":[{"path":target,"kind":"config"}],"exact_version_required":false});
    let (out, _) = f.run(&["install", "fixture:demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    let mut receipts = f.read_json("state/receipts.json")?;
    let scope = receipts["installations"]["fixture:demo"]["scope"].clone();
    receipts["installations"][id] = json!({"scope":scope,"package":package,"observation":{"status":"unknown","reason":"legacy_baseline_unavailable"},"operation_id":"OP-legacy","updated_at":0});
    super::write_json(&f.file("state/receipts.json"), &receipts)?;
    let (out, _) = f.run(&["adopt", id, "--yes"])?;
    assert_code(&out, 0);
    let receipt = fs::read(f.file("state/receipts.json"))?;
    let (out, value) = f.run(&["list", "--kind", "configs"])?;
    assert_code(&out, 0);
    assert_eq!(
        value["data"]["resources"][0]["baseline"]["comparison"],
        "same"
    );
    fs::write(&target, "foreign-edit\n")?;
    let (out, value) = f.run(&["list", "--kind", "configs"])?;
    assert_code(&out, 3);
    assert_eq!(
        value["data"]["resources"][0]["baseline"]["comparison"],
        "different"
    );
    assert_eq!(fs::read(f.file("state/receipts.json"))?, receipt);
    assert!(!f.home.join("native-called").exists());
    Ok(())
}

#[test]
fn complete_explicit_context_is_one_shot_and_native_state_remains_owned() -> TestResult {
    let f = Native::new()?;
    let target = f.destination.join(".demo");
    let out = f
        .fixture
        .command()
        .arg("--chezmoi-source")
        .arg(&f.source)
        .arg("--chezmoi-destination")
        .arg(&f.destination)
        .arg("--chezmoi-config")
        .arg(&f.config)
        .args(["config", "apply", path_text(&target)?, "--yes"])
        .output()?;
    assert_code(&out, 0);
    assert!(!f.fixture.file("state/chezmoi-owner.json").exists());
    let calls = f.calls()?;
    let (out, value) = f.run(&["config", "status", path_text(&target)?])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "chezmoi_binding_unresolved");
    assert_eq!(calls, f.calls()?);
    f.enroll()?;
    let owner = f.fixture.read_json("state/chezmoi-owner.json")?;
    assert_eq!(
        owner["persistent_state"],
        json!(f.fixture.home.join("chezmoistate.boltdb"))
    );
    assert_eq!(owner["cache"], json!(f.fixture.home.join("native-cache")));
    assert_ne!(
        owner["persistent_state"],
        json!(f.fixture.file("state/chezmoi-native.boltdb"))
    );
    Ok(())
}

#[test]
fn content_revisions_are_allowed_but_native_state_namespace_changes_require_enrollment()
-> TestResult {
    let f = Native::new()?;
    f.enroll()?;
    let target = f.destination.join(".demo");
    fs::write(&f.config, "# data revision\n[data]\nmessage='new'\n")?;
    let (out, value) = f.run(&["config", "source-path", path_text(&target)?])?;
    assert_code(&out, 0);
    assert_eq!(
        value["data"]["path"],
        json!(f.source.join("home/dot_demo.tmpl"))
    );
    fs::write(f.fixture.home.join("new-native-state"), "")?;
    fs::write(
        &f.config,
        "# state selection revision\n[data]\nmessage='new'\n",
    )?;
    let (out, value) = f.run(&["config", "apply", path_text(&target)?, "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "chezmoi_binding_changed");
    assert!(!target.exists());
    Ok(())
}

#[test]
fn native_configured_entry_filters_remain_partial_even_after_exit_zero() -> TestResult {
    let f = Native::new()?;
    fs::write(f.fixture.home.join("filter-files"), "")?;
    f.enroll()?;
    let target = f.destination.join(".demo");
    let (out, value) = f.run(&["config", "apply", path_text(&target)?, "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["nativeResult"], "succeeded");
    assert_eq!(value["data"]["phase"], "unknown");
    assert_eq!(value["data"]["verification"], "partial-filtered");
    assert_eq!(value["data"]["filesMatch"], Value::Null);
    Ok(())
}

#[test]
fn mise_owner_check_handles_unrefreshed_enrollment_and_keeps_disjoint_writes_available()
-> TestResult {
    for (mode, expected) in [
        ("managed", Some("configuration_owner_requires_review")),
        ("overlap", Some("config_authority_uncertain")),
        ("refreshed", None),
        ("late-modifier", Some("config_authority_uncertain")),
        ("late-encrypted", Some("config_authority_uncertain")),
        ("unrelated-modifier", None),
        ("late-external", Some("config_authority_uncertain")),
        ("late-remove", Some("config_authority_uncertain")),
        ("unrelated-external", None),
        ("disjoint", None),
    ] {
        let mut native = Native::new()?;
        if mode != "disjoint" {
            native.destination = native.fixture.home.clone();
        }
        if mode == "managed" {
            let file = native
                .source
                .join("home/dot_config/mise/private_config.toml");
            fs::create_dir_all(file.parent().ok_or("native source root")?)?;
            fs::write(file, b"[tools]\ndemo='1.0.0'\n")?;
        }
        let f = &native.fixture;
        let mut catalog = f.read_json("catalog.json")?;
        catalog["packages"][0]["provider"] = json!({"type":"mise","package":"demo"});
        super::write_json(&f.catalog, &catalog)?;
        let version = f.home.join("mise-version");
        fs::write(&version, b"1.0.0")?;
        f.script(
            "mise",
            r#"case "$1" in
ls) printf '[{"version":"%s","active":true}]' "$(/bin/cat "$HOME/mise-version")";;
use) [ "$2 $3 $4" = '--global --pin demo@2.0.0' ]; printf 2.0.0 > "$HOME/mise-version";;
*) exit 92;;
esac"#,
        )?;
        assert_code(&f.run(&["adopt", "demo", "--yes"])?.0, 0);
        native.enroll()?;
        if mode != "overlap" && mode != "disjoint" && mode != "managed" {
            fs::write(
                native.source.join("home/.chezmoiignore"),
                b"# refreshed control file\n",
            )?;
            assert_code(&native.run(&["config", "list", "--refresh"])?.0, 0);
        }
        let added = match mode {
            "late-modifier" => Some("home/dot_config/mise/modify_config.toml"),
            "late-encrypted" => Some("home/dot_config/mise/encrypted_private_config.toml.age"),
            "unrelated-modifier" => Some("home/modify_dot_bashrc"),
            "late-external" => Some("home/dot_config/mise/.chezmoiexternal.toml"),
            "late-remove" => Some("home/dot_config/mise/.chezmoiremove"),
            "unrelated-external" => Some("home/dot_config/other/.chezmoiexternal.toml"),
            _ => None,
        };
        if let Some(added) = added {
            let file = native.source.join(added);
            fs::create_dir_all(file.parent().ok_or("native source root")?)?;
            fs::write(file, b"# synthetic source control\n")?;
        }
        let calls = native.calls()?;
        let (stdout, result) = f.run(&["upgrade", "demo", "--version", "2.0.0"])?;
        if let Some(error) = expected {
            assert_code(&stdout, 3);
            assert_eq!(result["error"]["code"], error, "{result}");
            assert_eq!(fs::read(&version)?, b"1.0.0");
        } else {
            assert_code(&stdout, 0);
            assert_eq!(result["data"]["phase"], "succeeded");
            assert_eq!(fs::read(&version)?, b"2.0.0");
        }
        assert_eq!(native.calls()?, calls);
    }
    Ok(())
}
