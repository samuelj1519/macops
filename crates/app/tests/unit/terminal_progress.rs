#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::fs::{self, File, OpenOptions};

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
