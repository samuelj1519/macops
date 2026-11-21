#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::os::unix::ffi::OsStrExt;

#[test]
fn finite_source_names_match_ordered_official_subset_without_interpreting_contents() -> TestResult {
    for (name, directory, target, template, symlink) in [
        ("dot_config", false, ".config", false, false),
        (
            "private_readonly_executable_dot_tool.tmpl",
            false,
            ".tool",
            true,
            false,
        ),
        ("symlink_dot_config.tmpl", false, ".config", true, true),
        ("private_readonly_dot_config", true, ".config", false, false),
        ("dot_config.tmpl", true, ".config.tmpl", false, false),
        ("foo.tmpl.tmpl", false, "foo.tmpl", true, false),
        ("foo.TMPL", false, "foo.TMPL", false, false),
        ("dot_dot_x", false, ".dot_x", false, false),
        ("dot_run_task", false, ".run_task", false, false),
        ("普通配置", false, "普通配置", false, false),
        ("file.literal", true, "file.literal", false, false),
    ] {
        let mapped = source_name(name, directory)?.ok_or("candidate")?;
        assert_eq!(
            mapped,
            SourceName {
                target: target.into(),
                template,
                symlink
            }
        );
    }
    for name in [".git", ".chezmoiignore", ".raw-hidden"] {
        assert!(source_name(name, true)?.is_none());
    }
    for name in [
        "readonly_private_dot_x",
        "private_symlink_dot_x",
        "symlink_private_dot_x",
        "private_private_dot_x",
        "foo.literal.tmpl",
        "foo.tmpl.literal",
        "empty_dot_x",
        "run_once_before_task",
        "create_dot_file",
        "modify_dot_file",
        "encrypted_dot_g.age",
        "literal_foo",
    ] {
        assert_eq!(
            source_name(name, false).err().ok_or("unsupported")?.code,
            "unsupported_source_attribute"
        );
    }
    for name in [
        "external_folder",
        "exact_dir",
        "remove_dir",
        "executable_dot_x",
        "symlink_dot_x",
        "literal_dir",
    ] {
        assert!(source_name(name, true).is_err());
    }
    for name in ["", "dot_", "dot_.", "private_.", "a/b", "name\0value"] {
        assert!(source_name(name, false).is_err());
    }
    assert!(source_name("private_.", true).is_err());
    Ok(())
}

#[test]
fn root_fields_are_case_insensitive_typed_and_do_not_keep_unrelated_secret_values() -> TestResult {
    for (format,bytes) in [
        ("json",br#"{"SourceDir":"/source","DESTDIR":"~/destination","source_dir":"ignored","hooks":{"pre":"SYNTHETIC_SECRET"}}"#.as_slice()),
        ("toml",b"SourceDir='/source'\nDESTDIR='~/destination'\nsource_dir='ignored'\n[hooks]\npre='SYNTHETIC_SECRET'".as_slice()),
    ] {
        let fields = root_fields(bytes,format)?;
        assert_eq!(fields, RootFields { source:Some("/source".into()), destination:Some("~/destination".into()) });
        assert!(!format!("{fields:?}").contains("SYNTHETIC_SECRET"));
    }
    for (format, bytes) in [
        (
            "json",
            br#"{"sourceDir":"/one","SourceDir":"/two"}"#.as_slice(),
        ),
        ("json", br#"{"destDir":"/one","DESTdir":"/two"}"#.as_slice()),
        ("json", br#"{"sourceDir":null}"#.as_slice()),
        ("json", br#"{"sourceDir":true}"#.as_slice()),
        ("json", br#"["SYNTHETIC_SECRET"]"#.as_slice()),
        ("json", br#"{"hooks": bad SYNTHETIC_SECRET}"#.as_slice()),
        ("json", b"{SYNTHETIC_SECRET".as_slice()),
        ("toml", b"sourceDir='/one'\nSOURCEDIR='/two'".as_slice()),
        ("toml", b"sourceDir=true".as_slice()),
        ("toml", b"sourceDir='SYNTHETIC_SECRET\n".as_slice()),
        ("toml", b"\xff".as_slice()),
        ("yaml", b"sourceDir: /source".as_slice()),
    ] {
        let error = root_fields(bytes, format).err().ok_or("error")?;
        assert!(!error.to_string().contains("SYNTHETIC_SECRET"));
    }
    assert!(root_fields("{\"ſourceDir\":\"/source\"}".as_bytes(), "json").is_err());
    assert!(root_fields("'ſourceDir'='/source'".as_bytes(), "toml").is_err());
    assert_eq!(root_fields(b"{}", "json")?, RootFields::default());
    Ok(())
}

#[test]
fn root_paths_do_not_expand_environment_fall_back_or_escape_home_during_tilde_expansion()
-> TestResult {
    let home = Path::new("/fixture/home");
    assert_eq!(path_value("~/root", home)?, home.join("root"));
    assert_eq!(path_value("~//root", home)?, home.join("root"));
    assert_eq!(path_value("/absolute", home)?, PathBuf::from("/absolute"));
    for value in [
        "",
        "relative",
        "$HOME/root",
        "{{ output }}",
        "/a/../b",
        "/Users/example/job",
        "bad\0value",
    ] {
        assert!(path_value(value, home).is_err());
    }
    assert_eq!(xdg_home(None, home, ".config")?, home.join(".config"));
    assert_eq!(
        xdg_home(Some(OsStr::new("")), home, ".config")?,
        home.join(".config")
    );
    assert_eq!(
        xdg_home(Some(OsStr::new("/absolute")), home, ".config")?,
        PathBuf::from("/absolute")
    );
    assert!(xdg_home(Some(OsStr::new("relative")), home, ".config").is_err());
    assert_eq!(
        xdg_dirs(None, &["/one", "/two"])?,
        vec![PathBuf::from("/one"), PathBuf::from("/two")]
    );
    assert_eq!(
        xdg_dirs(Some(OsStr::new("")), &["/default"])?,
        vec![PathBuf::from("/default")]
    );
    assert_eq!(
        xdg_dirs(Some(OsStr::new("/one:/two")), &[])?,
        vec![PathBuf::from("/one"), PathBuf::from("/two")]
    );
    for value in ["/one:", ":/two", "relative", "/one:relative"] {
        assert!(xdg_dirs(Some(OsStr::new(value)), &[]).is_err());
    }
    assert!(xdg_dirs(Some(OsStr::new(&vec!["/root"; 65].join(":"))), &[]).is_err());
    let invalid = OsStr::from_bytes(b"/invalid-\xff");
    assert!(xdg_home(Some(invalid), home, ".config").is_err());
    assert!(xdg_dirs(Some(invalid), &[]).is_err());
    Ok(())
}

#[test]
fn root_directive_is_a_single_bounded_relative_selection_not_a_template() -> TestResult {
    assert_eq!(directive(b" home \n")?, PathBuf::from("home"));
    assert_eq!(directive(b".")?, PathBuf::from("."));
    for value in [
        b"".as_slice(),
        b"../outside",
        b"/absolute",
        b"foo/../outside",
        b"{{ output }}",
        b"$ROOT",
        b"bad\0value",
        b"\xff",
    ] {
        assert!(directive(value).is_err());
    }
    Ok(())
}

#[test]
fn scan_budget_counts_observed_entries_and_preserves_the_strongest_diagnostic() {
    let mut report = Coverage {
        source: "test".into(),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![],
        issues: vec![],
    };
    let mut budget = Budget::new(2, 4);
    assert_eq!(budget.depth, 4);
    assert!(budget.take(&mut report));
    assert_eq!(report.count, 1);
    assert!(budget.reserve(&mut report));
    assert_eq!(report.count, 1);
    assert!(!budget.reserve(&mut report));
    assert!(!budget.take(&mut report));
    assert_eq!(report.count, 1);
    assert_eq!(report.state, CoverageState::Truncated);
    for state in [
        CoverageState::Unsupported,
        CoverageState::Absent,
        CoverageState::Complete,
    ] {
        issue(&mut report, state, "less severe".into());
        assert_eq!(report.state, CoverageState::Truncated);
    }
    issue(
        &mut report,
        CoverageState::Unavailable,
        "unavailable".into(),
    );
    assert_eq!(report.state, CoverageState::Unavailable);
    issue(&mut report, CoverageState::Invalid, "invalid".into());
    assert_eq!(report.state, CoverageState::Invalid);
}
