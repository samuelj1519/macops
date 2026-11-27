#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::fs::{self, File, OpenOptions};

#[test]
fn a_native_output_banner_omits_local_recipe_arguments() -> TestResult {
    use crate::domain::{Action, CommandSpec, Kind, Observation, Package, Plan, Provider};
    use clap::Parser;
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let cli = crate::cli::Cli::try_parse_from([
        "mo",
        "--home",
        temp.path().to_str().ok_or("fixture path")?,
        "app",
    ])?;
    let ctx = crate::context::Context::from_cli(&cli, &mut vec![])?;
    let userinfo = ["actor", "fixture-only"].join(":");
    let url = format!("https://{userinfo}@example.org/file");
    let command = CommandSpec {
        program: "/fixture/bin/brew".into(),
        args: vec!["install".into(), "--cask".into(), url.clone()],
    };
    let package = Package {
        id: "fixture:cask".into(),
        name: "fixture".into(),
        kind: Kind::App,
        official_url: String::new(),
        provider: Provider::Brew {
            package: url,
            cask: true,
        },
        paths: vec![],
        exact_version_required: false,
    };
    let journal = crate::operations::new_journal(
        &ctx,
        Plan {
            brew: None,
            npm_binding: None,
            uv_owner: None,
            timeout_secs: 60,
            recipe_sha256: crate::domain::fingerprint(&package)?,
            package,
            action: Action::Install,
            before: Observation::Missing { evidence: None },
            target: None,
            command: Some(command.clone()),
            query: None,
            warnings: vec![],
            synchronous: true,
            delegated: false,
        },
    )?;
    let text = super::super::plan::native_banner(&journal, &command, None);
    assert!(text.contains("原工具输出 · Homebrew") && text.contains("/fixture/bin/brew"));
    assert!(!text.contains("fixture-only"));
    Ok(())
}

#[test]
fn progress_keeps_the_same_circle_and_label_start_with_and_without_color() {
    let label = "核验新市场与插件";
    for (phase, activity, suffix) in [
        (ProgressPhase::Working(0), "·", ""),
        (ProgressPhase::Working(1), "•", ""),
        (ProgressPhase::Working(3), " ", ""),
        (ProgressPhase::Complete, " ", "  已完成"),
        (ProgressPhase::Incomplete, " ", "  未完成"),
    ] {
        let line = progress_line(label, phase, 50, false);
        assert_eq!(line, format!("│ ○ {activity}  {label}{suffix}"));
    }
    let narrow = progress_line(label, ProgressPhase::Incomplete, 18, false);
    assert_eq!(narrow, "│ ○    核…  未完成");
    assert_eq!(progress_line(label, ProgressPhase::Complete, 1, false), "○");
    let colored = progress_line(label, ProgressPhase::Complete, 50, true);
    assert!(colored.contains("\x1b[1;32m○\x1b[0m"));
}

#[test]
fn native_progress_reassembles_split_unicode_and_strips_control_sequences() -> TestResult {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("terminal");
    let input = temp.path().join("native");
    let mut append = OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(&input)?;
    let native = File::open(&input)?;
    let error = tempfile::tempfile()?;
    let mut display = LiveProgress {
        terminal: File::create(&output)?,
        streams: Default::default(),
        queued: vec![],
        visible: false,
    };
    let chinese = "更新".as_bytes();
    append.write_all(&chinese[..4])?;
    display.relay([&native, &error], false);
    assert_eq!(fs::read_to_string(&output)?, "更");

    append.write_all(&chinese[4..])?;
    append.write_all(b"\r\x1b[2Jnative\n")?;
    display.relay([&native, &error], true);
    let status = fs::read_to_string(&output)?;
    assert!(status.starts_with("更新\r"));
    assert!(status.contains("native\n") && !status.contains('\x1b'));
    drop(display);
    assert_eq!(fs::read_to_string(output)?, format!("{status}\n"));
    Ok(())
}

#[test]
fn live_output_bounds_a_blocked_terminal_queue() -> TestResult {
    let temp = tempfile::NamedTempFile::new()?;
    let mut display = LiveProgress {
        terminal: File::open(temp.path())?,
        streams: Default::default(),
        queued: vec![],
        visible: false,
    };
    let large = vec![b'x'; 128 * 1024];
    assert!(display.display(&large));
    assert!(!display.display(b"unbounded"));
    assert_eq!(display.queued.len(), large.len());
    assert_eq!(temp.as_file().metadata()?.len(), 0);
    Ok(())
}
