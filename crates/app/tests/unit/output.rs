#![cfg(test)]
use super::*;
use serde_json::json;

#[test]
fn upgrade_summary_names_the_failed_and_unattempted_skills() {
    let output = skill_journey(&json!({
        "skillAction": "upgrade",
        "incomplete": true,
        "results": [{"name":"alpha","recipe":"internal-alpha","error":{"message":"更新前发现目录已改变"}}],
        "unattempted": ["internal-beta"],
        "unattemptedNames": ["beta"]
    })).unwrap_or_default();
    assert!(output.contains("alpha · 更新前发现目录已改变"));
    assert!(output.contains("尚未开始　beta"));
    assert!(!output.contains("internal-"));
}

#[test]
fn operation_followups_and_native_observations_use_their_user_facing_status() {
    for required in [true, false] {
        let stage = serde_json::json!({
            "journal":{"phase":"succeeded"},
            "continuationRequired":required,
            "continuationCommand":"mo agent resume OP-example"
        });
        let text = crate::operations::recovery_text(&stage, false);
        assert!(!text.contains("结果 已完成"));
        assert!(text.contains(if required {
            "mo agent resume OP-example"
        } else {
            "只证明原阶段已完成"
        }));
    }
    let no_command = serde_json::json!({
        "journal":{"phase":"succeeded"},
        "continuationRequired":true
    });
    assert!(crate::operations::recovery_text(&no_command, false).contains("运行 mo recover"));
    assert_eq!(status_text("brew-missing"), "没找到");
    assert_eq!(status_text("brew-installed"), "已找到");
}

#[test]
fn human_keeps_resource_ids_and_hides_operation_ids() {
    let text =
        human(&json!([{"id":"brew:demo","name":"demo","kind":"cli","provider":{"type":"brew"}}]));
    assert!(text.contains("brew:demo\tcli\tbrew\tdemo"));
    assert_eq!(
        human(&json!({"id":"OP-1","phase":"unknown","message":"需要恢复"})),
        "结果 还没确认\n需要恢复\n"
    );
    assert!(human(&json!({"mode":"dry-run","command":["demo"]})).contains("未执行写后端"));
}

#[test]
fn human_fallbacks_and_legacy_output_keep_information() {
    assert!(human(&json!([{"id":"demo","name":"demo"}])).contains("demo\t-\t-\tdemo"));
    assert_eq!(human(&json!([])), "ID\t类型\t维护渠道\t名称\n");
    assert!(human(&json!({"journal":{"id":"OP-1","phase":"succeeded","message":"done"},"output":"original backend output"})).ends_with("original backend output"));
    assert!(
        human(&json!({"journal":{"id":"OP-1","phase":"succeeded","message":"done"}}))
            .contains("done")
    );
    assert_eq!(
        human(&json!({"data":{"text":"legacy text"}})),
        "legacy text"
    );
    assert_eq!(human(&json!(true)), "true\n");
    assert!(human(&json!({"data":null})).contains("null"));
}

fn journey(args: &[&str], value: &Value) -> Result<String, clap::Error> {
    use clap::Parser;
    let cli = crate::cli::Cli::try_parse_from(std::iter::once("mo").chain(args.iter().copied()))?;
    Ok(command_human(
        TextLayout {
            columns: 80,
            terminal: false,
        },
        &cli.command,
        value,
    ))
}

fn assert_journeys(cases: &[(&[&str], Value, &str)]) -> crate::test_support::TestResult {
    for &(args, ref value, expected) in cases {
        let text = journey(args, value)?;
        assert!(text.contains(expected), "{args:?}: {text}");
        assert!(text.contains("--json"), "{args:?}: {text}");
    }
    Ok(())
}

#[test]
fn software_and_operations_show_the_next_action() -> crate::test_support::TestResult {
    assert_journeys(&[
        (
            &["history", "OP-fixture"],
            json!([{"id":"OP-fixture","phase":"unknown"}]),
            "操作历史",
        ),
        (
            &["dirs", "--roots"],
            json!({"state":"/fixture/state"}),
            "状态 /fixture/state",
        ),
        (
            &["manifest", "plan", "manifest.json"],
            json!({"entries":[{"id":"npm:one","replay":"read-only"}]}),
            "read-only",
        ),
        (
            &["manifest", "replay", "manifest.json", "npm:one"],
            json!({"status":"unchanged","id":"npm:one"}),
            "现有安装已满足清单",
        ),
        (
            &["cleanup", "macops:cache"],
            json!({"candidates":[{"name":"owned-index"}],"review":["external"]}),
            "external",
        ),
        (
            &["migrate", "preflight"],
            json!({"ready":false,"blocked":[{"path":"/fixture/state","reason":"pending"}]}),
            "具备迁移条件 否",
        ),
        (
            &["migrate", "recover", "OP-fixture"],
            json!({"phase":"succeeded","migration_id":"OP-fixture","result":"rolled-back"}),
            "迁移编号 OP-fixture",
        ),
        (
            &["app", "doctor"],
            json!([{"id":"npm:one","observation":{"status":"missing"}}]),
            "npm:one\t没找到",
        ),
        (&["app", "outdated"], json!([]), "mo app refresh"),
        (
            &["app", "upgrade"],
            json!({"mode":"dry-run","results":[{"id":"npm:one","available":"2.0"}]}),
            "软件维护预演",
        ),
        (
            &["app", "upgrade"],
            json!({"results":[{"id":"npm:one","operation":{"journal":{"id":"OP-one","phase":"unknown"}}}],"unattempted":["npm:two"]}),
            "未执行 npm:two",
        ),
        (
            &["app", "service", "list"],
            json!({"service":{"name":"postgresql"},"status":"stopped"}),
            "状态 未启动",
        ),
        (
            &["app", "discover"],
            json!([{"manager":"brew","status":"unknown","error":{"code":"fixture"}}]),
            "管理器发现",
        ),
        (
            &["app", "discover", "npm"],
            json!({"manager":"npm","packages":[{"id":"npm:demo","name":"demo"}]}),
            "发现的安装",
        ),
    ])
}

#[test]
fn configuration_and_skill_results_show_coverage_and_scope() -> crate::test_support::TestResult {
    assert_journeys(&[
        (
            &["config", "list"],
            json!({"coverage":"unresolved","incomplete":true}),
            "先运行 mo config enroll",
        ),
        (
            &["config", "enroll"],
            json!({"coverage":"owner-enrolled"}),
            "mo config list --refresh",
        ),
        (
            &["config", "list"],
            json!({"coverage":"cached","candidates":{"shown":["/fixture/file"],"count":1,"truncated":false}}),
            "总数 1",
        ),
        (
            &["config", "where", "file"],
            json!({"coverage":"cached","matches":{"shown":["/fixture/file"]}}),
            "/fixture/file",
        ),
        (
            &["config", "verify", "/fixture/file"],
            json!({"results":[],"coverage":"partial","incomplete":true}),
            "有些内容没查到",
        ),
        (
            &["config", "diff"],
            json!({"results":[{"path":"/fixture/file","local":"M","desired":"A"}]}),
            "/fixture/file\tM\tA",
        ),
        (
            &["config", "status"],
            json!({"results":[{"path":"/fixture/file","local":"M","desired":"A"}]}),
            "/fixture/file\tM\tA",
        ),
        (
            &["config", "source-path"],
            json!({"path":"/fixture/source"}),
            "路径 /fixture/source",
        ),
        (
            &["config", "list", "--dry-run"],
            json!({"mode":"dry-run","resolution":"unresolved","evidence":{"coverage":"cached"}}),
            "仅使用缓存",
        ),
        (
            &["skill", "plugins", "list"],
            json!({"results":[{"instance":"codex:one","loaded":"unknown"}]}),
            "会话载入",
        ),
        (
            &["skill", "plugins", "check", "fixture"],
            json!({"results":[{"instance":"codex","version":"1.2.3","registration":"registered","configured_enabled":false,"payload":"present","loaded":"unknown"}]}),
            "1.2.3\tregistered\t否\tpresent\t还不能确认",
        ),
        (
            &["skill", "upgrade", "one"],
            json!({"results":[{"journal":{"id":"OP-skill","phase":"running"}}],"unattempted":["second"]}),
            "重试时会先检查这次结果",
        ),
        (
            &["skill", "sync", "--all"],
            json!({"plan":{"changes":[{"path":"/fixture/link","action":"link"}]}}),
            "/fixture/link\tlink",
        ),
        (
            &["skill", "policy", "check"],
            json!({"status":"same","changes":[]}),
            "状态 same",
        ),
        (
            &["config", "reconcile"],
            json!({"reconcile_state":"drift","rows":[{"key":"codex","live":"1","declared":"2"}]}),
            "codex\t1\t2",
        ),
        (
            &["skill", "refresh", "--dry-run"],
            json!({"recipes":[{"install_name":"demo","source":{"commit":"abc"}}]}),
            "demo\tabc",
        ),
        (
            &["skill", "outdated"],
            json!({"results":[{"name":"demo","status":"unknown","reason":"index_missing_or_stale"}]}),
            "index_missing_or_stale",
        ),
        (
            &["skill", "refresh"],
            json!({"results":[{"name":"demo","status":"unknown","reason":"index_missing_or_stale"}]}),
            "index_missing_or_stale",
        ),
    ])
}

#[test]
fn agent_and_discovery_results_show_identity_and_ownership() -> crate::test_support::TestResult {
    assert_journeys(&[
        (
            &["agent", "launch", "one", "--dry-run"],
            json!({"profile":"one","workspace":"/fixture/work","preference":"enabled"}),
            "工作目录 /fixture/work",
        ),
        (
            &["agent", "credential", "list"],
            json!([{"profile":"mi","status":"missing"}]),
            "档案凭据状态",
        ),
        (
            &["agent", "credential", "add", "mi", "two"],
            json!({"command":{"program":"/fixture/a'b","args":["credential","add","mi","two"]}}),
            "'/fixture/a'\\''b' 'credential'",
        ),
        (
            &["agent", "tools"],
            json!({"tools":[{"tool":"codex","version":"1.0.0"}]}),
            "当前版本",
        ),
        (
            &["search", "codex"],
            json!({"tool":{"tool":"codex","version":"1.0.0"}}),
            "工具 codex",
        ),
        (
            &["search", "demo"],
            json!({"candidates":[{"name":"demo","category":"application","id":"npm:demo"}]}),
            "使用所列 ID 查询",
        ),
        (&["app", "resources"], json!({"resources":[]}), "资源清单"),
        (
            &["search", "demo"],
            json!({"package":{"name":"demo","id":"npm:demo"},"observation":{"status":"missing"}}),
            "本机观察",
        ),
        (
            &["agent", "upgrade"],
            json!({"reconcile_state":"drift","rows":[{"key":"codex","live":"1","declared":"2"}]}),
            "Agent 版本配置核对",
        ),
    ])
}

#[test]
fn previews_and_resource_details_keep_selected_evidence_and_escape_terminal_input()
-> crate::test_support::TestResult {
    let plan = json!({"mode":"dry-run","plan":{"package":{"name":"工具","id":"npm:demo","provider":{"type":"npm","prefix":"/fixture/npm"}},"target":"2.0.0","warnings":["确认范围"],"command":{"args":["FROZEN_PRIVATE_ARGUMENT"]}}});
    let preview = journey(&["app", "install", "demo", "--dry-run"], &plan)?;
    assert!(preview.contains("安装范围 /fixture/npm") && preview.contains("计划版本 2.0.0"));
    assert!(preview.contains("确认范围") && !preview.contains("FROZEN_PRIVATE_ARGUMENT"));
    assert!(
        journey(
            &["agent", "upgrade", "codex"],
            &json!({"mode":"dry-run","plan":{"tool":"codex","force_live":true}})
        )?
        .contains("只更新本机 是")
    );
    assert!(
        journey(&["manifest", "export"], &json!({"manifest":{"entries":[]}}))?
            .contains("\"manifest\"")
    );
    let pending = journey(
        &["app", "install", "demo"],
        &json!({"journal":{"id":"OP-one","phase":"prepared"}}),
    )?;
    assert!(pending.contains("运行 mo recover 可查看记录") && !pending.contains("OP-one"));
    assert!(original_operation(&json!({"id":"OP-bad\nname","phase":"unknown"})).is_none());
    assert!(original_operation(&json!({"id":"OP-done","phase":"succeeded"})).is_none());
    assert_eq!(
        original_operation(&json!({"operation_id":"OP-policy","status":"unknown"})),
        Some("OP-policy")
    );
    let applied = journey(
        &["config", "apply", "/fixture/file"],
        &json!({"operation_id":"OP-native","phase":"unknown","nativeResult":"succeeded",
            "verification":"partial-filtered","nativeExternalEffects":"unverified"}),
    )?;
    assert!(applied.contains("运行 mo recover 可查看记录") && !applied.contains("OP-native"));
    assert!(applied.contains("外部效果 还不能确认"));
    let restored = journey(
        &["config", "restore", "OP-native"],
        &json!({"mode":"no-op","operation_id":"OP-native","changed":false}),
    )?;
    assert!(restored.contains("配置已是还原后的状态") && !restored.contains("OP-native"));

    for (kind, expected) in [
        ("application", "本机版本"),
        ("agent", "档案目录"),
        ("capability", "部署范围"),
        ("config", "候选来源"),
    ] {
        let resource = json!({"resource":{"kind":kind,"name":"对象\x1b","config_dir":"/fixture/config","configured_enabled":false}});
        let text = journey(&["search", "sample"], &resource)?;
        assert!(text.contains("对象\\u{1b}"), "{text}");
        assert!(!text.contains('\x1b'));
        assert!(text.contains(expected));
    }
    assert_eq!(sanitize("中文\n\u{202a}\t", true), "中文\n\\u{202a}\\t");
    assert_eq!(sanitize("中文\n\u{2066}", false), "中文\\n\\u{2066}");
    Ok(())
}

#[test]
fn specialized_previews_and_fallbacks_keep_unknown_states_explicit()
-> crate::test_support::TestResult {
    assert_journeys(&[
        (
            &["config", "diff", "--dry-run"],
            json!({"mode":"dry-run","resolution":"unresolved","evidence":{"coverage":"cached"}}),
            "实际目标尚待解析",
        ),
        (
            &["config", "adopt", "/fixture/file", "--dry-run"],
            json!({"mode":"dry-run","backend":"chezmoi-source","target":"/fixture/file","sourceParent":"/fixture/source","effect":"plain-source-file"}),
            "计划影响 plain-source-file",
        ),
        (
            &["skill", "install", "--recipe", "fixture.json", "--dry-run"],
            json!({"mode":"dry-run","recipe":{"install_name":"demo","scope":"global","source":{"commit":"abc"}}}),
            "来源提交 abc",
        ),
        (
            &["skill", "plugins", "upgrade", "codex:demo", "--dry-run"],
            json!({"mode":"dry-run","native_id":"demo@market","recipe":"codex:demo"}),
            "实例 demo@market",
        ),
        (
            &["app", "upgrade", "demo", "--dry-run"],
            json!({"mode":"dry-run","backend":"brew-native","scope":{"prefix":"/fixture","full_name":"demo","cellar":"/fixture/Cellar"},"unresolved":true}),
            "版本待核验 是",
        ),
    ])?;
    let journal = json!({"id":"OP-one","phase":"running"});
    for args in [
        &["config", "restore", "OP-one"][..],
        &["app", "start", "postgresql"],
        &["skill", "policy", "recover"],
    ] {
        let text = journey(args, &journal)?;
        assert!(text.contains("运行 mo recover 可查看记录"), "{text}");
        assert!(!text.contains("OP-one"));
    }
    let diagnostic = json!({"mode":"dry-run","backend":"other","opaque":true});
    assert!(journey(&["skill", "outdated"], &diagnostic)?.contains("\"opaque\""));
    assert!(journey(&["app", "refresh", "--dry-run"], &diagnostic)?.contains("\"opaque\""));
    Ok(())
}

#[test]
fn plain_errors_explain_the_failure_and_only_show_supported_usage() {
    use crate::error::Error;
    let cases = [
        ("recovery_required", "mo recover OP-123"),
        ("migration_recovery_required", "mo recover OP-123"),
        ("confirmation_required", "mo recover OP-123"),
        ("mark_failed_unsupported", "mo recover OP-123"),
        ("mise_config_drift", "mo recover OP-123"),
        ("model_keychain_update_pending", "mo recover OP-123"),
        ("operation_busy", "等另一个操作结束"),
        ("operation_still_running", "等上一次操作结束"),
        ("operation_in_flight", "等上一次操作结束"),
        ("plan_stale", "看清变化后再继续"),
        ("recovery_scope_changed", "原来的目录选项"),
        ("operation_conflict", "原来的目录选项"),
        ("operation_finished", "mo history"),
        ("invalid_operation_id", "OP- 开头的编号"),
        ("not_found", "再核对位置"),
        ("ambiguous", "清单里的编号"),
        ("adopt_target_required", "清单里的编号"),
        ("chezmoi_binding_unresolved", "mo config enroll"),
        ("model_keychain_unavailable", "mo recommend --setup"),
        ("model_keychain_timeout", "本地分析"),
        ("model_profile_missing", "mo recommend --setup"),
        ("exact_version_required", "完整版本号"),
        ("invalid_version", "完整版本号"),
        ("project_scope_unsupported", "项目里的对象"),
        ("uncategorized", "mo recover OP-123"),
    ];
    for (code, next) in cases {
        let error = Error::attention(code, "先看原因\nmo recover OP-123\x1b");
        let text = error_text(&error);
        assert!(text.starts_with("错误表现　命令未完成\n错误原因　先看原因\n"));
        assert!(text.contains(next), "{text}");
        assert!(text.contains(&format!("（{code}）")));
        assert!(!text.contains("记下刚才的命令"));
        assert!(!text.contains('\x1b'));
    }
    let path = error_text(&Error::new("absolute_path_required", "relative"));
    assert!(path.contains("收到「relative」") && path.contains("本机绝对路径"));
    let unknown = error_text(&Error::new("uncategorized", "没有更多可判断的信息"));
    assert!(!unknown.contains("正确用法"));
    let guided = Error::new("fixture", "问题原因").with_guidance("问题表现", "适用命令");
    assert_eq!(
        error_text(&guided),
        "错误表现　问题表现\n错误原因　问题原因\n正确用法　适用命令"
    );
    assert_eq!(
        error_text(&Error::attention("cancelled", "fixture")),
        "已取消。"
    );
    assert_eq!(human(&json!({"mode":"cancelled"})), "已取消。\n");
    assert_eq!(human(&json!({"status":"cancelled"})), "已取消。\n");
}

#[test]
fn human_status_words_do_not_relabel_names_or_make_unknown_results_sound_finished() {
    for (code, expected) in [
        ("unknown", "还不能确认"),
        ("unverified", "还不能确认"),
        ("drift", "有差异"),
        ("different", "有差异"),
        ("missing", "没找到"),
        ("present", "已找到"),
        ("stopped", "未启动"),
        ("unchanged", "不用改"),
        ("not-needed", "不用改"),
        ("blocked", "暂时不能继续"),
        ("partial-filtered", "有些文件没检查（按设置筛选）"),
        ("prepared", "已准备"),
        ("running", "正在运行"),
        ("failed", "失败"),
        ("succeeded", "已完成"),
        ("current", "current"),
    ] {
        assert_eq!(
            cell(&json!({"status":code}), "/status").as_deref(),
            Some(expected)
        );
        assert_eq!(cell(&json!({"name":code}), "/name").as_deref(), Some(code));
    }
}

#[test]
fn result_details_distinguish_a_saved_phase_from_a_live_status_and_explain_failed_items()
-> crate::test_support::TestResult {
    for (code, label) in [("running", "上次记录为正在运行"), ("failed", "已记录失败")]
    {
        assert_eq!(
            cell(&json!({"phase":code}), "/phase").as_deref(),
            Some(label)
        );
    }
    let data = json!({"results":[
        {"id":"app:one","status":"unknown","error":{"message":"权限不足"}},
        {"recipe":"recipe:two","status":"unknown","error":{"message":"没找到文件"}},
        {"status":"unknown","error":{"message":"没有记录"}}
    ]});
    let text = journey(&["app", "upgrade"], &data)?;
    assert!(text.contains("app:one · 权限不足"));
    assert!(text.contains("recipe:two · 没找到文件"));
    assert!(text.contains("没有记录"));
    Ok(())
}

#[test]
fn terminal_feedback_distinguishes_mo_progress_with_or_without_color() {
    assert_eq!(
        terminal_line(TerminalLine::Heading, "安装", false),
        "mo  安装"
    );
    assert!(terminal_line(TerminalLine::Step, "核对", true).contains("\x1b[36m"));
    let note = terminal_line(TerminalLine::Note, "诊断\x1b", false);
    assert!(!note.contains('\x1b'));
    assert!(note.contains("诊断"));
    assert!(terminal_line(TerminalLine::Done, "已完成", false).contains("✓"));
}

#[test]
fn agent_uninstall_reports_the_archive_location_for_preview_and_completion()
-> crate::test_support::TestResult {
    for (mode, title) in [("dry-run", "卸载预览"), ("live", "已归档")] {
        let data = json!({"mode":mode,"profile":"Codex · example","location":"/fixture/profile","archive":"/fixture/.archived-profile"});
        let summary = agent_summary(
            TextLayout {
                columns: 80,
                terminal: false,
            },
            &AgentCmd::Uninstall {
                profile: "fixture".into(),
            },
            &data,
        )
        .ok_or("archive summary")?;
        assert!(summary.contains(title));
        assert!(summary.contains("归档目录 /fixture/.archived-profile"));
    }
    Ok(())
}

#[test]
fn command_tables_use_the_supplied_terminal_width() {
    let layout = TextLayout {
        columns: 40,
        terminal: true,
    };
    let text = command_human(
        layout,
        &Command::History { query: None },
        &json!([{"id": "OP-fixture", "plan": {"package": {"name": "配置"}, "action": "install"}}]),
    );
    assert!(text.contains("操作号"));
    assert!(text.contains("OP-fixture"));
    assert!(text.contains("配置"));
    assert!(!text.contains('\t'));
}

#[test]
fn agent_batch_summary_distinguishes_work_done_from_existing_state() {
    let mut data = json!({
        "agentBatch": true,
        "verb": "升级",
        "results": [
            {"agent": "claude-mi", "status": "current"},
            {"agent": "claude-glm", "status": "changed"},
            {"agent": "claude-kimi", "status": "not-installed"},
        ],
    });
    let layout = TextLayout {
        columns: 80,
        terminal: false,
    };
    let command = Command::Search {
        query: "fixture".into(),
    };
    let text = command_human(layout, &command, &data);
    assert!(text.contains("2/2 个适用 Agent 已达到目标"));
    assert!(text.contains("本次修改 1 · 已达目标 1 · 未安装 1"));
    data["mode"] = json!("dry-run");
    assert!(command_human(layout, &command, &data).starts_with("预演 · 升级\n"));
}
