use crate::cli::{AgentCmd, AppCmd, Command, Config, Manifest, SkillCmd};
use serde_json::Value;
use std::fmt::Write;

type Fields = &'static [(&'static str, &'static str)];

const ID_NAME: Fields = &[("/id", "ID"), ("/name", "名称")];
const RESOURCES: Fields = &[
    ("/name", "名称"),
    ("/id", "ID"),
    ("/status", "状态"),
    ("/management", "管理方式"),
];
const TOOLS: Fields = &[
    ("/tool", "工具"),
    ("/version", "当前版本"),
    ("/live_pin", "本机固定版本"),
    ("/declared_pin", "源固定版本"),
    ("/status", "状态"),
];
const OBSERVATIONS: Fields = &[
    ("/id", "ID"),
    ("/observation/status", "观察"),
    ("/observation/version", "版本"),
    ("/baseline/comparison", "基线"),
    ("/observation/reason", "原因"),
];
const VERSIONS: Fields = &[
    ("/id", "ID"),
    ("/installed/version", "当前版本"),
    ("/available", "可用版本"),
    ("/version", "索引版本"),
    ("/status", "状态"),
    ("/error/code", "原因"),
];
const JOURNALS: Fields = &[
    ("/id", "操作号"),
    ("/plan/package/name", "目标"),
    ("/plan/action", "动作"),
    ("/phase", "阶段"),
    ("/state", "状态目录"),
];
const JSON_HINT: &str = "完整字段可用 --json 查看。\n";

pub(crate) fn sanitize(value: &str, multiline: bool) -> String {
    let mut clean = String::with_capacity(value.len());
    for ch in value.chars() {
        if (ch != '\n' || !multiline)
            && (ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        {
            clean.extend(ch.escape_default());
        } else {
            clean.push(ch);
        }
    }
    clean
}

#[derive(Clone, Copy)]
pub(crate) enum TerminalLine {
    Heading,
    Section,
    Step,
    Done,
    Note,
}

pub(crate) fn terminal_line(kind: TerminalLine, text: &str, color: bool) -> String {
    let (marker, code) = match kind {
        TerminalLine::Heading => ("mo", "1;36"),
        TerminalLine::Section => ("──", "1;36"),
        TerminalLine::Step => ("›", "36"),
        TerminalLine::Done => ("✓", "32"),
        TerminalLine::Note => ("!", "33"),
    };
    let text = sanitize(text, false);
    if color {
        format!("\x1b[{code}m{marker:>2}\x1b[0m  {text}")
    } else {
        format!("{marker:>2}  {text}")
    }
}

pub(crate) fn error_text(error: &crate::error::Error) -> String {
    if error.code == "cancelled" {
        return "已取消。".into();
    }
    if let Some(guidance) = &error.guidance {
        return format!(
            "错误表现　{}\n错误原因　{}\n正确用法　{}",
            sanitize(&guidance.problem, false),
            sanitize(&error.message, true),
            sanitize(&guidance.usage, true)
        );
    }
    let next = match error.code.as_str() {
        "operation_busy" => "等另一个操作结束，再用 mo recover 看有没有要检查的结果。",
        "operation_still_running" | "operation_in_flight" => "请先等上一次操作结束。",
        "plan_stale" | "recovery_scope_changed" | "operation_conflict" => {
            "先用原来的目录选项查看当前记录。看清变化后再继续。"
        }
        "operation_finished" => "用 mo history 查看这次操作的结果。",
        "invalid_operation_id" => "运行 mo recover 查看还要检查的操作，再复制 OP- 开头的编号。",
        "not_found" | "ambiguous" | "adopt_target_required" => {
            "打开 mo，按 / 搜索名字，再核对位置。同名项可以用清单里的编号区分。"
        }
        "chezmoi_binding_unresolved" => "先运行 mo config enroll，告诉 mo 要管理哪一份设置。",
        "model_keychain_unavailable" | "model_keychain_timeout" | "model_profile_missing" => {
            "运行 mo recommend --setup 检查模型设置，也可以选择本地分析。"
        }
        "exact_version_required" | "invalid_version" => {
            "先查看这条命令的 --help，再填入完整版本号，例如 1.2.3。"
        }
        "project_scope_unsupported" => {
            "--project 可用来看项目里的对象。先在列表里找到要操作的对象。"
        }
        "absolute_path_required" => "把路径改为以 / 开头的本机绝对路径，再运行原命令。",
        _ => "",
    };
    let reason = if error.code == "absolute_path_required" {
        format!(
            "此处需要本机绝对路径，收到「{}」。",
            sanitize(&error.message, false)
        )
    } else {
        sanitize(&error.message, true)
    };
    let mut text = format!(
        "错误表现　命令未完成\n错误原因　{reason}（{}）",
        sanitize(&error.code, false)
    );
    if !next.is_empty() {
        let _ = write!(text, "\n正确用法　{next}");
    }
    text
}

pub(crate) fn phase_text(phase: &str) -> &'static str {
    match phase {
        "prepared" => "已准备",
        "running" => "上次记录为正在运行",
        "failed" => "已记录失败",
        "succeeded" => "已完成",
        _ => "还没确认",
    }
}

fn status_text(value: &str) -> &str {
    match value {
        "unknown" | "unverified" => "还不能确认",
        "drift" | "different" => "有差异",
        "missing" | "brew-missing" => "没找到",
        "present" | "brew-installed" => "已找到",
        "stopped" => "未启动",
        "running" => "正在运行",
        "failed" => "失败",
        "unchanged" | "not-needed" => "不用改",
        "blocked" => "暂时不能继续",
        "partial-filtered" => "有些文件没检查（按设置筛选）",
        "prepared" | "succeeded" => phase_text(value),
        _ => value,
    }
}

fn cell(value: &Value, key: &str) -> Option<String> {
    let value = value.pointer(key)?;
    match value {
        Value::String(value) => {
            let text = match key.rsplit('/').next() {
                Some("phase") if matches!(value.as_str(), "running" | "failed") => {
                    phase_text(value)
                }
                Some(
                    "status"
                    | "phase"
                    | "comparison"
                    | "reconcile_state"
                    | "nativeResult"
                    | "verification"
                    | "nativeExternalEffects"
                    | "loaded",
                ) => status_text(value),
                _ => value,
            };
            Some(sanitize(text, false))
        }
        Value::Bool(value) => Some(if *value { "是" } else { "否" }.into()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn record(title: &str, value: &Value, fields: Fields) -> String {
    let mut text = format!("{title}\n");
    for &(key, label) in fields {
        if let Some(value) = cell(value, key) {
            let _ = writeln!(text, "  {label} {value}");
        }
    }
    text
}

fn table(title: &str, value: &Value, fields: Fields) -> String {
    let mut text = format!("{title}\n");
    let rows = value.as_array().map_or(&[][..], Vec::as_slice);
    if rows.is_empty() {
        text.push_str("  本次没有记录。\n");
        return text;
    }
    let _ = writeln!(
        text,
        "{}",
        fields
            .iter()
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join("\t")
    );
    for row in rows {
        let cells: Vec<_> = fields
            .iter()
            .map(|(key, _)| cell(row, key).unwrap_or_else(|| "—".into()))
            .collect();
        let _ = writeln!(text, "{}", cells.join("\t"));
    }
    text
}

fn resource_detail(value: &Value) -> String {
    let fields: Fields = match value["kind"].as_str() {
        Some("application") => &[
            ("/manager", "维护渠道"),
            ("/scope", "安装范围"),
            ("/recipe_ref", "配方"),
            ("/baseline/comparison", "基线"),
        ],
        Some("agent") => &[
            ("/client", "客户端"),
            ("/config_dir", "档案目录"),
            ("/model/selected", "模型"),
            ("/runtime/binary", "程序"),
        ],
        Some("capability") => &[
            ("/delivery_kind", "交付类型"),
            ("/origin/label", "来源"),
            ("/revision/recorded", "记录版本"),
            ("/configured_enabled", "配置启用"),
            ("/loaded", "会话载入"),
        ],
        _ => &[
            ("/target", "实际路径"),
            ("/owner", "管理方"),
            ("/observed_type", "文件类型"),
        ],
    };
    let extra = match value["kind"].as_str() {
        Some("application") => table(
            "本机版本",
            &value["releases"],
            &[
                ("/version", "版本"),
                ("/path", "位置"),
                ("/active", "正在使用"),
            ],
        ),
        Some("capability") => table(
            "部署范围",
            &value["deployments"],
            &[
                ("/harness", "客户端"),
                ("/scope", "范围"),
                ("/path", "位置"),
            ],
        ),
        Some("config") => table(
            "候选来源",
            &value["source_candidates"],
            &[("/path", "路径")],
        ),
        _ => String::new(),
    };
    record("对象详情", value, RESOURCES) + &record("范围与证据", value, fields) + &extra
}

fn preview(data: &Value) -> Option<String> {
    let plan = &data["plan"];
    if data["backend"] == "brew-native" {
        return Some(record(
            "Homebrew 操作预演（未运行原生查询）",
            data,
            &[
                ("/id", "目标"),
                ("/scope/full_name", "Formula"),
                ("/scope/prefix", "Homebrew 前缀"),
                ("/scope/cellar", "Cellar"),
                ("/target", "计划版本"),
                ("/unresolved", "版本待核验"),
                ("/reason", "依据"),
            ],
        ));
    }
    if plan["tool"].is_string() {
        return Some(record(
            "Agent CLI 变更预演（未执行写入）",
            plan,
            &[
                ("/tool", "工具"),
                ("/version", "计划版本"),
                ("/force_live", "只更新本机"),
            ],
        ));
    }
    if data["recipe"].is_object() {
        return Some(record(
            "技能配方预演（未执行写入）",
            &data["recipe"],
            &[
                ("/install_name", "安装名"),
                ("/scope", "范围"),
                ("/source/repository", "仓库"),
                ("/source/commit", "来源提交"),
                ("/manual_only", "仅手动触发"),
            ],
        ));
    }
    if data["native_id"].is_string() && data["recipe"].is_string() {
        return Some(record(
            "插件配方预演（未执行写入）",
            data,
            &[("/native_id", "实例"), ("/recipe", "配方")],
        ));
    }
    plan["package"].is_object().then(|| {
        let mut text = record(
            "操作预演（未执行写入）",
            plan,
            &[
                ("/package/name", "目标"),
                ("/package/id", "ID"),
                ("/action", "动作"),
                ("/package/provider/type", "维护渠道"),
                ("/package/provider/prefix", "安装范围"),
                ("/package/provider/query/full_name", "Formula"),
                ("/package/provider/query/prefix", "Homebrew 前缀"),
                ("/package/provider/query/cellar", "Cellar"),
                ("/before/status", "当前状态"),
                ("/before/version", "当前版本"),
                ("/target", "计划版本"),
            ],
        );
        for warning in plan["warnings"].as_array().into_iter().flatten() {
            let _ = writeln!(text, "  提示 {}", cell(warning, "").unwrap_or_default());
        }
        text
    })
}

pub(crate) fn original_operation(data: &Value) -> Option<&str> {
    let row = data.get("journal").unwrap_or(data);
    let id = row["id"]
        .as_str()
        .or_else(|| row["operation_id"].as_str())
        .filter(|id| crate::policy::operation::state::valid_id(id))?;
    matches!(
        row["phase"].as_str().or_else(|| row["status"].as_str()),
        Some("prepared" | "running" | "unknown")
    )
    .then_some(id)
}

pub(crate) fn recovery_hint(data: &Value) -> Option<&'static str> {
    original_operation(data).map(|_| "重试时会先检查这次结果。运行 mo recover 可查看记录。")
}

fn batches(title: &str, data: &Value, fields: Fields) -> String {
    let mut text = table(title, &data["results"], fields);
    for row in data["results"].as_array().into_iter().flatten() {
        if let Some(hint) = recovery_hint(row.get("operation").unwrap_or(row)) {
            let _ = writeln!(text, "  {hint}");
        }
        if let Some(message) = row["error"]["message"].as_str() {
            let target = cell(row, "/id")
                .or_else(|| cell(row, "/recipe"))
                .unwrap_or_default();
            let _ = writeln!(text, "  {target} · {}", sanitize(message, false));
        }
    }
    if let Some(skipped) = data["unattempted"].as_array() {
        for value in skipped {
            let _ = writeln!(text, "  未执行 {}", cell(value, "").unwrap_or_default());
        }
    }
    text
}

fn config_summary(command: &Config, data: &Value) -> Option<String> {
    match command {
        Config::Reconcile { .. } if data["reconcile_state"].is_string() => Some(reconcile(data)),
        Config::Restore { .. } if data["mode"] == "no-op" => {
            Some("配置已是还原后的状态。\n".into())
        }
        Config::List { .. } | Config::Enroll | Config::Where { .. }
            if data["coverage"].is_string() =>
        {
            let mut text = record(
                "配置范围",
                data,
                &[
                    ("/roots/effective_source", "管理源"),
                    ("/roots/destination", "目标目录"),
                    ("/coverage", "证据范围"),
                    ("/observedAt", "观察时间（Unix 秒）"),
                ],
            );
            match data["coverage"].as_str() {
                Some("unresolved") => text.push_str("先运行 mo config enroll 登记范围。\n"),
                Some("owner-enrolled") => {
                    text.push_str("已登记。运行 mo config list --refresh 读取受管路径。\n");
                }
                _ => {
                    let paths = data.get("matches").unwrap_or(&data["candidates"]);
                    text.push_str(&table("已观察路径", &paths["shown"], &[("", "路径")]));
                    text.push_str(&record(
                        "显示范围",
                        paths,
                        &[("/count", "总数"), ("/truncated", "截断")],
                    ));
                }
            }
            Some(text)
        }
        Config::Status { .. } | Config::Diff { .. } | Config::Verify { .. }
            if data["results"].is_array() =>
        {
            let mut text = table(
                "配置差异方向",
                &data["results"],
                &[
                    ("/path", "路径"),
                    ("/local", "本机"),
                    ("/desired", "管理源"),
                ],
            );
            text.push_str(&record(
                "证据",
                data,
                &[("/coverage", "覆盖"), ("/matched", "核验一致")],
            ));
            Some(text)
        }
        Config::SourcePath { .. } if data["path"].is_string() => Some(record(
            "配置管理源",
            data,
            &[("/path", "路径"), ("/coverage", "证据")],
        )),
        Config::Adopt { .. } | Config::Forget { .. }
            if data["backend"] == "chezmoi-source" && data["mode"] == "dry-run" =>
        {
            Some(record(
                "配置管理预演（未执行写入）",
                data,
                &[
                    ("/target", "实际文件"),
                    ("/action", "动作"),
                    ("/sourceParent", "管理源目录"),
                    ("/sourceName", "管理源名称"),
                    ("/effect", "计划影响"),
                ],
            ))
        }
        _ if data["resolution"] == "unresolved" && data["mode"] == "dry-run" => Some(record(
            "配置预演（仅使用缓存，实际目标尚待解析）",
            data,
            &[
                ("/evidence/roots/effective_source", "管理源"),
                ("/evidence/roots/destination", "目标目录"),
                ("/evidence/coverage", "证据范围"),
            ],
        )),
        _ => None,
    }
}

fn reconcile(data: &Value) -> String {
    record(
        "Agent 版本配置核对",
        data,
        &[
            ("/reconcile_state", "状态"),
            ("/reason", "原因"),
            ("/live", "本机配置"),
            ("/source", "管理源"),
            ("/spec", "工具"),
            ("/from", "原版本"),
            ("/to", "目标版本"),
        ],
    ) + &table(
        "版本差异",
        &data["rows"],
        &[("/key", "工具"), ("/live", "本机"), ("/declared", "管理源")],
    )
}

fn credentials(data: &Value) -> String {
    if let Some(program) = data["command"]["program"].as_str() {
        let args = data["command"]["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        let words: Vec<_> = std::iter::once(program)
            .chain(args)
            .map(|word| format!("'{}'", sanitize(word, false).replace('\'', "'\\''")))
            .collect();
        return format!("凭据由原生工具交互处理\n  {}\n", words.join(" "));
    }
    table(
        "档案凭据状态",
        data,
        &[
            ("/profile", "档案"),
            ("/account", "账户"),
            ("/status", "状态"),
            ("/reason", "原因"),
        ],
    )
}

fn specialized(command: &Command, data: &Value) -> Option<String> {
    match command {
        Command::History { .. } => Some(table("操作历史", data, JOURNALS)),
        Command::Dirs { roots: true, .. } => Some(record(
            "当前目录范围",
            data,
            &[
                ("/home", "用户目录"),
                ("/config", "配置"),
                ("/data", "数据"),
                ("/state", "状态"),
                ("/cache", "缓存"),
                ("/catalog", "配方目录"),
                ("/settings", "设置"),
            ],
        )),
        Command::Manifest {
            command: Manifest::Plan { .. },
        } => Some(table(
            "清单差异与重放条件",
            &data["entries"],
            &[
                ("/id", "目标"),
                ("/current_version", "当前版本"),
                ("/requested_version", "期望版本"),
                ("/status", "差异"),
                ("/replay", "重放条件"),
            ],
        )),
        Command::Manifest {
            command: Manifest::Replay { .. },
        } if data["status"] == "unchanged" => Some(record(
            "现有安装已满足清单",
            data,
            &[("/id", "目标"), ("/version", "版本")],
        )),
        Command::Cleanup { .. } if data["candidates"].is_array() => Some(
            table(
                "可清理的自有缓存",
                &data["candidates"],
                &[("/name", "文件")],
            ) + &table("保留供核对", &data["review"], &[("", "文件")]),
        ),
        Command::Migrate { .. } if data["blocked"].is_array() => Some(
            record("迁移预检（只读）", data, &[("/ready", "具备迁移条件")])
                + &table(
                    "阻断项",
                    &data["blocked"],
                    &[("/path", "路径"), ("/reason", "原因")],
                ),
        ),
        Command::Migrate { .. } if data["migration_id"].is_string() => Some(record(
            "迁移记录",
            data,
            &[
                ("/migration_id", "迁移编号"),
                ("/mode", "动作"),
                ("/phase", "阶段"),
                ("/result", "结果"),
                ("/authority", "数据来源"),
            ],
        )),
        Command::App {
            command: Some(command),
            ..
        } => app_summary(command, data),
        Command::Config {
            command: Some(command),
            ..
        } => config_summary(command, data),
        Command::Skill {
            command: Some(command),
            ..
        } => skill_summary(command, data),
        Command::Agent {
            command: Some(command),
            ..
        } => agent_summary(command, data),
        _ => None,
    }
}

fn app_summary(command: &AppCmd, data: &Value) -> Option<String> {
    match command {
        AppCmd::Discover { .. } if data.is_array() => Some(table(
            "管理器发现",
            data,
            &[
                ("/manager", "管理器"),
                ("/status", "状态"),
                ("/coverage/state", "覆盖"),
                ("/error/code", "原因"),
            ],
        )),
        AppCmd::Discover { .. } if data["packages"].is_array() => Some(
            record(
                "管理器发现",
                data,
                &[
                    ("/manager", "管理器"),
                    ("/status", "状态"),
                    ("/coverage/state", "覆盖"),
                ],
            ) + &table("发现的安装", &data["packages"], RESOURCES),
        ),
        AppCmd::Doctor { .. } if data.is_array() => Some(table("软件诊断", data, OBSERVATIONS)),
        AppCmd::Outdated { .. } if data.is_array() => {
            Some(table("可用版本检查", data, VERSIONS) + "索引无证据时运行 mo app refresh。\n")
        }
        AppCmd::Upgrade { id: None, .. } | AppCmd::Refresh { .. } if data["results"].is_array() => {
            Some(batches(
                if data["mode"] == "dry-run" {
                    "软件维护预演"
                } else {
                    "软件维护结果"
                },
                data,
                VERSIONS,
            ))
        }
        AppCmd::Service { .. } | AppCmd::Start { .. } | AppCmd::Stop { .. }
            if data.get("service").is_some() =>
        {
            Some(record(
                "服务状态",
                data,
                &[
                    ("/service", "服务"),
                    ("/service/name", "服务"),
                    ("/status", "状态"),
                    ("/observation/status", "观察"),
                    ("/changed", "本次有变更"),
                ],
            ))
        }
        _ => None,
    }
}

fn skill_summary(command: &SkillCmd, data: &Value) -> Option<String> {
    match command {
        SkillCmd::Refresh(_) if data["recipes"].is_array() => Some(table(
            "技能索引预演",
            &data["recipes"],
            &[("/install_name", "安装名"), ("/source/commit", "来源提交")],
        )),
        SkillCmd::Refresh(_) | SkillCmd::Outdated { .. } if data["results"].is_array() => {
            Some(table(
                "技能版本证据",
                &data["results"],
                &[
                    ("/id", "ID"),
                    ("/name", "名称"),
                    ("/status", "状态"),
                    ("/reason", "原因"),
                ],
            ))
        }
        SkillCmd::Plugins { .. } if data["results"].is_array() => Some(table(
            "本地插件证据",
            &data["results"],
            &[
                ("/instance", "实例"),
                ("/status", "状态"),
                ("/declared_version", "声明版本"),
                ("/version", "观察版本"),
                ("/registration", "原生登记"),
                ("/configured_enabled", "配置启用"),
                ("/payload", "载荷"),
                ("/loaded", "会话载入"),
                ("/reason", "原因"),
            ],
        )),
        SkillCmd::Upgrade(_) if data["results"].is_array() => {
            let rows = data["results"].as_array()?;
            if rows.iter().all(|row| row["status"] == "fixed-version") && !rows.is_empty() {
                let mut text = String::from("所选技能固定在当前版本。\n");
                let repositories: std::collections::BTreeSet<_> = rows
                    .iter()
                    .filter_map(|row| row["repository"].as_str())
                    .collect();
                for source in repositories {
                    let _ = writeln!(
                        text,
                        "查看其他版本　mo recommend {}",
                        sanitize(source, false)
                    );
                }
                return Some(text);
            }
            Some(batches(
                "技能维护结果",
                data,
                &[
                    ("/journal/plan/package/name", "目标"),
                    ("/name", "技能"),
                    ("/recipe", "配方"),
                    ("/recipe/install_name", "安装名"),
                    ("/recipe/source/commit", "来源提交"),
                    ("/mode", "动作"),
                    ("/journal/phase", "阶段"),
                    ("/status", "状态"),
                    ("/error/code", "原因"),
                ],
            ))
        }
        SkillCmd::Sync(_) | SkillCmd::Policy { .. }
            if data.get("status").is_some() || data.get("plan").is_some() =>
        {
            Some(
                record(
                    "技能与档案策略",
                    data,
                    &[("/status", "状态"), ("/scope", "范围")],
                ) + &table(
                    "变更路径",
                    data.get("plan")
                        .map_or(&data["changes"], |plan| &plan["changes"]),
                    &[("/path", "目标"), ("/action", "动作")],
                ),
            )
        }
        _ => None,
    }
}

fn agent_summary(command: &AgentCmd, data: &Value) -> Option<String> {
    match command {
        AgentCmd::Upgrade { tool: None, .. } if data["reconcile_state"].is_string() => {
            Some(reconcile(data))
        }
        AgentCmd::Credential { .. } => Some(credentials(data)),
        AgentCmd::Launch { .. } if data["profile"].is_string() => Some(record(
            "Agent 启动预演（未启动会话）",
            data,
            &[
                ("/profile", "档案"),
                ("/workspace", "工作目录"),
                ("/nativeProfile", "原生档案"),
                ("/plugin", "插件"),
                ("/preference", "启动偏好"),
            ],
        )),
        AgentCmd::Uninstall { .. } => Some(record(
            if data["mode"] == "dry-run" {
                "单独配置卸载预览"
            } else {
                "单独配置已归档"
            },
            data,
            &[
                ("/profile", "档案"),
                ("/location", "当前目录"),
                ("/archive", "归档目录"),
            ],
        )),
        _ => None,
    }
}

pub(crate) fn command_human(command: &Command, data: &Value) -> String {
    if data["agentRegistration"] == true {
        let status = match data["status"].as_str() {
            Some("preview") => "登记预演",
            Some("already-registered") => "已登记",
            _ => "登记完成",
        };
        return format!(
            "{status} · {}\n配置目录　{}\n",
            sanitize(field(data, "name"), false),
            sanitize(field(data, "config"), false)
        );
    }
    if data["agentBatch"] == true
        && let Some(rows) = data["results"].as_array()
    {
        let text = crate::skills::portfolio::batch_text(rows);
        let achieved = rows
            .iter()
            .filter(|row| matches!(row["status"].as_str(), Some("changed" | "current")))
            .count();
        let unchanged = rows.iter().filter(|row| row["status"] == "current").count();
        let uninstalled = rows
            .iter()
            .filter(|row| row["status"] == "not-installed")
            .count();
        return if data["mode"] == "dry-run" {
            format!("预演 · {}\n{text}\n", field(data, "verb"))
        } else {
            format!(
                "{text}\n{achieved}/{} 个适用 Agent 已达到目标 · 本次修改 {} · 已达目标 {unchanged} · 未安装 {uninstalled}\n",
                rows.len() - uninstalled,
                achieved - unchanged
            )
        };
    }
    if let Some(text) = skill_journey(data) {
        return text;
    }
    let summary = specialized(command, data)
        .or_else(|| (data["mode"] == "dry-run").then(|| preview(data)).flatten())
        .or_else(|| data.get("resource").map(resource_detail))
        .or_else(|| {
            data["resources"]
                .is_array()
                .then(|| table("资源清单", &data["resources"], RESOURCES))
        })
        .or_else(|| {
            matches!(command, Command::Search { .. })
                .then(|| data.get("candidates"))
                .flatten()
                .map(|rows| {
                    table(
                        "找到多个对象，请使用所列 ID 查询",
                        rows,
                        &[("/category", "类型"), ("/name", "名称"), ("/id", "ID")],
                    )
                })
        })
        .or_else(|| {
            data.get("package").map(|package| {
                record("对象详情", package, ID_NAME)
                    + &record(
                        "本机观察",
                        &data["observation"],
                        &[
                            ("/status", "状态"),
                            ("/version", "版本"),
                            ("/reason", "原因"),
                        ],
                    )
                    + &record(
                        "管理证据",
                        data,
                        &[("/owner", "管理方"), ("/loaded", "会话载入")],
                    )
            })
        })
        .or_else(|| {
            data["tools"]
                .is_array()
                .then(|| table("Agent CLI 版本", &data["tools"], TOOLS))
        })
        .or_else(|| {
            data["tool"]
                .is_object()
                .then(|| record("Agent CLI 详情", &data["tool"], TOOLS))
        });
    let show_detail = summary.is_some();
    let mut text = summary.unwrap_or_else(|| human(data));
    if data["incomplete"] == true {
        text.push_str("有些内容没查到。这里只显示已经找到的结果。\n");
    }
    if show_detail {
        text.push_str(JSON_HINT);
    }
    if let Some(hint) = recovery_hint(data) {
        let _ = writeln!(text, "{hint}");
    }
    text
}

pub(crate) fn skill_journey(data: &Value) -> Option<String> {
    let (title, done) = match data["skillAction"].as_str()? {
        "install" => ("安装技能", "已安装并接管"),
        "adopt" => ("接管技能", "已接管，保留现有技能文件"),
        "upgrade" => ("更新技能", "已更新"),
        _ => return None,
    };
    let mut text = String::new();
    if data["status"] == "already-managed" {
        return Some("这些技能已经接管。可以直接升级。\n".into());
    }
    if data["status"] == "up-to-date" {
        return Some("这些技能已是所选版本。\n".into());
    }
    if data["mode"] == "dry-run" {
        let _ = writeln!(text, "{title} · 预览");
        for (key, label) in [
            ("available", "可安装"),
            ("skills", "可接管"),
            ("alreadyPresent", "已有"),
        ] {
            if let Some(values) = data[key].as_array().filter(|items| !items.is_empty()) {
                let names = values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|name| sanitize(name, false))
                    .collect::<Vec<_>>();
                let _ = writeln!(text, "{label}　{}", names.join("、"));
            }
        }
        if let Some(directory) = data["directory"].as_str() {
            let _ = writeln!(text, "位置　{}", sanitize(directory, false));
        }
        return Some(text);
    }
    for result in data["results"].as_array().into_iter().flatten() {
        if result["journal"]["phase"] == "succeeded" {
            let name = field(&result["journal"]["plan"]["package"], "name");
            let _ = writeln!(text, "✓  {} · {done}", sanitize(name, false));
        } else {
            let name = result["name"]
                .as_str()
                .or(result["journal"]["plan"]["package"]["name"].as_str())
                .unwrap_or("技能");
            let reason = result["error"]["message"]
                .as_str()
                .or(result["journal"]["message"].as_str())
                .unwrap_or("结果待核对");
            let _ = writeln!(
                text,
                "!  {} · {}",
                sanitize(name, false),
                sanitize(reason, false)
            );
        }
    }
    if data["incomplete"] == true {
        if let Some(items) = data
            .get("unattemptedNames")
            .unwrap_or(&data["unattempted"])
            .as_array()
            .filter(|items| !items.is_empty())
        {
            let names: Vec<_> = items
                .iter()
                .filter_map(Value::as_str)
                .map(|name| sanitize(name, false))
                .collect();
            let _ = writeln!(text, "尚未开始　{}", names.join("、"));
        }
        if data["results"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| recovery_hint(&item["journal"]).is_some())
        }) {
            text.push_str("重试时会先核对上一次的结果。查看记录可运行 mo recover。\n");
        }
    }
    if let Some(source) = data["source"].as_str().or(data["target"].as_str())
        && data["incomplete"] == false
    {
        let _ = writeln!(
            text,
            "更新　{}",
            sanitize(
                data["next"]
                    .as_str()
                    .map_or_else(|| format!("mo skill upgrade {source}"), str::to_owned)
                    .as_str(),
                false
            )
        );
    }
    Some(text)
}

pub fn human(data: &Value) -> String {
    if let Some(text) = skill_journey(data) {
        return text;
    }
    if data["mode"] == "cancelled" || data["status"] == "cancelled" {
        return "已取消。\n".into();
    }
    if let Some(rows) = data.as_array()
        && rows
            .iter()
            .all(|r| r.get("id").is_some() && r.get("name").is_some())
    {
        let mut text = String::from("ID\t类型\t维护渠道\t名称\n");
        for row in rows {
            let _ = writeln!(
                text,
                "{}\t{}\t{}\t{}",
                field(row, "id"),
                field(row, "kind"),
                row.get("provider").map_or("-", |p| field(p, "type")),
                field(row, "name")
            );
        }
        return text;
    }
    if let Some(journal) = data.get("journal") {
        return human(journal) + data.get("output").and_then(Value::as_str).unwrap_or("");
    }
    if data.get("phase").is_some() {
        let mut text = format!("结果 {}\n", phase_text(field(data, "phase")));
        if let Some(message) = data["message"]
            .as_str()
            .filter(|message| !message.is_empty())
        {
            let _ = writeln!(text, "{}", sanitize(message, true));
        }
        for (key, label) in [
            ("/nativeResult", "原生结果"),
            ("/verification", "文件核验"),
            ("/nativeExternalEffects", "外部效果"),
        ] {
            if let Some(value) = cell(data, key) {
                let _ = writeln!(text, "{label} {value}");
            }
        }
        return text;
    }
    if data.get("mode").and_then(Value::as_str) == Some("dry-run") {
        return format!(
            "预演：未执行写后端，未创建持久状态。\n{}\n",
            serde_json::to_string_pretty(data).unwrap_or_default()
        );
    }
    if let Some(text) = data
        .get("data")
        .and_then(|d| d.get("text"))
        .and_then(Value::as_str)
    {
        return text.to_owned();
    }
    serde_json::to_string_pretty(data).unwrap_or_default() + "\n"
}

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("-")
}

#[cfg(test)]
#[path = "../tests/unit/output.rs"]
mod tests;
