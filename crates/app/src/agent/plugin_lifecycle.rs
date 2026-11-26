use super::{native_plugins_files::Passive, resource_agents::InstallTarget};
use crate::{
    context::Context,
    domain::resource::CapabilityAction as Operation,
    domain::{
        Action, CommandSpec, Journal, Kind, Observation, Package, Phase, Plan, Provider,
        fingerprint,
    },
    error::{Error, Result},
    operations::{
        native_runner::{self, FrozenInvocation, NativeOutcome},
        store::Store,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const MARKER: &str = "agent-plugin-action-v1:";

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Desired {
    Absent,
    Enabled(bool),
    Version {
        version: String,
        enabled: Option<bool>,
        payload: String,
    },
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Facts {
    installed: bool,
    version: Option<String>,
    revision: Option<String>,
    enabled: Option<bool>,
    footprint: Option<String>,
    origin: Option<String>,
    payload: Option<String>,
}

impl Desired {
    fn met(&self, facts: &Facts) -> bool {
        match self {
            Self::Absent => !facts.installed,
            Self::Enabled(value) => facts.installed && facts.enabled == Some(*value),
            Self::Version {
                version,
                enabled,
                payload,
            } => {
                facts.installed
                    && facts.version.as_ref() == Some(version)
                    && facts.enabled == *enabled
                    && facts.payload.as_ref() == Some(payload)
            }
        }
    }

    fn action(&self) -> Action {
        match self {
            Self::Absent => Action::Uninstall,
            Self::Version { .. } => Action::Upgrade,
            Self::Enabled(_) => Action::Update,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Intent {
    target: InstallTarget,
    id: String,
    desired: Desired,
    before: Facts,
    binary: PathBuf,
    binary_digest: String,
    market: Option<Market>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Market {
    path: PathBuf,
    version: String,
    digest: String,
    payload: String,
}

fn unavailable(reason: impl Into<String>) -> Error {
    Error::attention("native_plugin_action_unsupported", reason)
}

fn leaf(id: &str) -> Result<(&str, &str)> {
    id.rsplit_once('@')
        .filter(|(name, market)| {
            super::native_plugins::valid_name(name) && super::native_plugins::valid_name(market)
        })
        .ok_or_else(|| unavailable("请用 插件名@市场名 确认是哪一个插件。"))
}

fn file(reader: &mut Passive, path: &Path) -> Result<Option<Vec<u8>>> {
    crate::platform::fs::write_path(path)?;
    reader.file(path)
}

fn read_facts(target: &InstallTarget, id: &str) -> Result<Facts> {
    let (name, market) = leaf(id)?;
    let root = target.open()?;
    let mut reader = Passive::new()?;
    let cache = root.path.join("plugins/cache").join(market).join(name);
    crate::platform::fs::write_path(&cache)?;
    let facts = match target.client.as_str() {
        "claude" => read_claude_facts(&root.path, &cache, id, name, market, &mut reader)?,
        "codex" => read_codex_facts(&root.path, &cache, id, &mut reader)?,
        _ => return Err(unavailable("该客户端没有已核验的原生插件维护命令。")),
    };
    reader.verify()?;
    root.verify()?;
    Ok(facts)
}

fn read_claude_facts(
    root: &Path,
    cache: &Path,
    id: &str,
    name: &str,
    market: &str,
    reader: &mut Passive,
) -> Result<Facts> {
    let market_book = file(reader, &root.join("plugins/known_marketplaces.json"))?
        .map(|bytes| crate::policy::json::parse(&bytes))
        .transpose()?;
    let origin = market_book
        .as_ref()
        .and_then(|value| value.get(market))
        .map(fingerprint)
        .transpose()?;
    let registered = file(reader, &root.join("plugins/installed_plugins.json"))?
        .map(|bytes| crate::policy::catalog::resources::plugin_book(&bytes))
        .transpose()?;
    let settings = file(reader, &root.join("settings.json"))?
        .map(|bytes| crate::policy::json::parse(&bytes))
        .transpose()?
        .unwrap_or_default();
    let enabled = settings["enabledPlugins"]
        .get(id)
        .map(|flag| {
            flag.as_bool()
                .ok_or_else(|| unavailable("该插件的启停配置无法核对。"))
        })
        .transpose()?;
    let records = registered
        .as_ref()
        .and_then(|book| book.plugins.get(id))
        .map_or(&[][..], Vec::as_slice);
    Ok(match records {
        [] => Facts {
            installed: false,
            version: None,
            revision: None,
            enabled,
            footprint: None,
            origin,
            payload: None,
        },
        [record]
            if record.scope == "user"
                && record.project.is_none()
                && record.path.starts_with(cache)
                && record.path != *cache =>
        {
            let payload = crate::platform::fs::Directory::open(&record.path)?;
            if payload.path != record.path {
                return Err(unavailable("插件载荷位置已变化。"));
            }
            let manifest = file(reader, &record.path.join(".claude-plugin/plugin.json"))?
                .ok_or_else(|| unavailable("插件载荷缺少清单。"))?;
            let identity = crate::policy::json::parse(&manifest)?;
            if identity["name"] != name || identity["version"].as_str() != record.version.as_deref()
            {
                return Err(unavailable("插件登记的名称或版本和实际文件不一致。"));
            }
            Facts {
                installed: true,
                version: record.version.clone(),
                revision: record.revision.clone(),
                enabled,
                payload: Some(crate::agent::ai_payload::native_plugin_tree_digest(
                    &payload, None,
                )?),
                origin,
                footprint: Some(fingerprint(&(
                    payload.identity(),
                    crate::capability::capability_store::hash(&manifest),
                ))?),
            }
        }
        _ => {
            return Err(unavailable(
                "这个插件还有项目级安装，或用户级记录不唯一。请在 Claude Code 的 /plugin 中选定范围。",
            ));
        }
    })
}
fn read_codex_facts(root: &Path, cache: &Path, id: &str, reader: &mut Passive) -> Result<Facts> {
    let settings = file(reader, &root.join("config.toml"))?
        .map(|bytes| crate::policy::catalog::resources::codex_plugins(&bytes))
        .transpose()?
        .unwrap_or_default();
    let enabled = settings.get(id).copied().flatten();
    let payload = match crate::platform::fs::Directory::open(cache) {
        Ok(directory) => Some(fingerprint(directory.identity())?),
        Err(error) if error.code == "path_missing" => None,
        Err(error) => return Err(error),
    };
    Ok(Facts {
        installed: settings.contains_key(id) || payload.is_some(),
        version: None,
        revision: None,
        enabled,
        footprint: payload,
        origin: None,
        payload: None,
    })
}
// Updating a single plugin is safe only when the registered local catalog
// already names the exact intended payload. Refreshing/removing a market
// would change other plugins and is a separate operation.
fn pinned_market(target: &InstallTarget, id: &str, expected: Option<&str>) -> Result<Market> {
    let (plugin, market) = leaf(id)?;
    let resolution = || {
        unavailable("现有市场没有核验到目标版本。请在这套 Claude Code 的 /plugin 中检查市场来源。")
    };
    let root = target.open()?;
    let mut reader = Passive::new()?;
    let known = file(
        &mut reader,
        &root.path.join("plugins/known_marketplaces.json"),
    )?
    .ok_or_else(resolution)?;
    let value = crate::policy::json::parse(&known)?;
    let source = &value[market]["source"];
    let Some(path) = source["path"]
        .as_str()
        .filter(|_| source["source"] == "directory")
    else {
        return Err(resolution());
    };
    let path = PathBuf::from(path);
    if value[market]["installLocation"].as_str() != path.to_str() {
        return Err(resolution());
    }
    let checkout = crate::platform::fs::Directory::open(&path)?;
    if checkout.path != path {
        return Err(unavailable(format!(
            "市场 {market} 指向本地目录 {}。请在这套 Claude Code 的 /plugin 中核对来源。",
            path.display()
        )));
    }
    let catalog =
        file(&mut reader, &path.join(".claude-plugin/marketplace.json"))?.ok_or_else(resolution)?;
    let manifest =
        file(&mut reader, &path.join(".claude-plugin/plugin.json"))?.ok_or_else(resolution)?;
    let catalog_value = crate::policy::json::parse(&catalog)?;
    let plugin_value = crate::policy::json::parse(&manifest)?;
    let version = plugin_value["version"]
        .as_str()
        .filter(|v| crate::policy::operation::manifest::valid_version(v))
        .ok_or_else(resolution)?;
    let entries = catalog_value["plugins"].as_array().ok_or_else(resolution)?;
    if catalog_value["name"] != market
        || plugin_value["name"] != plugin
        || !entries.iter().any(|entry| {
            entry["name"] == plugin
                && entry["source"] == "./"
                && entry.get("version").is_none_or(|value| value == version)
        })
    {
        return Err(resolution());
    }
    if let Some(expected) = expected
        && expected != version
    {
        return Err(unavailable(format!(
            "市场 {market} 指向本地目录 {}，提供的是 {version}。请在这套 Claude Code 的 /plugin 中核对目标版本 {expected} 的来源。",
            path.display()
        )));
    }
    let payload = crate::agent::ai_payload::native_plugin_tree_digest(&checkout, None)?;
    let digest = fingerprint(&(
        checkout.identity(),
        &payload,
        crate::capability::capability_store::hash(&catalog),
    ))?;
    checkout.verify()?;
    reader.verify()?;
    root.verify()?;
    Ok(Market {
        path,
        version: version.into(),
        digest,
        payload,
    })
}

fn digest_binary(path: &Path) -> Result<String> {
    let directory = crate::platform::fs::Directory::open(path.parent().ok_or_else(changed)?)?;
    if directory.path != path.parent().ok_or_else(changed)? {
        return Err(changed());
    }
    crate::agent::ai_payload::native_binary_digest(
        &directory,
        path.file_name().ok_or_else(changed)?,
    )
}

fn changed() -> Error {
    unavailable("所选插件、配置目录或执行程序已经变化。请重新核对后再试。")
}

pub(crate) fn matches_install(
    ctx: &Context,
    target: &InstallTarget,
    id: &str,
    version: &str,
    commit: &str,
) -> Result<bool> {
    if target.client != "claude" {
        return Ok(false);
    }
    let current = read_facts(target, id)?;
    if !current.installed || current.version.as_deref() != Some(version) {
        return Ok(false);
    }
    let market = pinned_market(target, id, Some(version))?;
    let source = super::claude_install::registered_checkout_source(
        ctx,
        &target.directory.canonical,
        id,
        &market.path,
    )?;
    Ok(source.is_some_and(|(_, actual)| actual == commit)
        && current.payload.as_ref() == Some(&market.payload))
}

impl Intent {
    pub(crate) fn key(&self) -> (u64, u64, String) {
        let identity = &self.target.directory.identity;
        (
            identity.device,
            identity.inode,
            format!("{}:{}:{:?}", self.target.client, self.id, self.desired),
        )
    }

    pub(crate) fn plan(
        ctx: &Context,
        target: InstallTarget,
        id: &str,
        operation: Operation,
        offered: Option<(&str, &str)>,
        require_offer: bool,
    ) -> Result<Option<Self>> {
        let before = read_facts(&target, id)?;
        let mut market = None;
        let desired = match operation {
            Operation::Uninstall => Desired::Absent,
            Operation::Enable => Desired::Enabled(true),
            Operation::Disable => Desired::Enabled(false),
            Operation::Upgrade => {
                if require_offer && offered.is_none() {
                    return Err(unavailable("未核验到插件目标版本。请核对仓库和版本。"));
                }
                if target.client != "claude" {
                    return Err(unavailable(
                        "当前 Codex 插件升级需要固定的原生配方。请用对应 Codex 的插件菜单检查。",
                    ));
                }
                let pinned = pinned_market(&target, id, offered.map(|(version, _)| version))?;
                if let Some((_, commit)) = offered {
                    let verified = super::claude_install::registered_checkout_source(
                        ctx,
                        &target.directory.canonical,
                        id,
                        &pinned.path,
                    )?;
                    if verified.is_none_or(|(_, actual)| actual != commit) {
                        return Err(unavailable(format!(
                            "市场指向本地目录 {}。目标提交未核验，请在这套 Claude Code 的 /plugin 中检查来源。",
                            pinned.path.display()
                        )));
                    }
                }
                let payload = pinned.payload.clone();
                let version = pinned.version.clone();
                market = Some(pinned);
                Desired::Version {
                    version,
                    enabled: before.enabled,
                    payload,
                }
            }
            Operation::Install => return Err(changed()),
        };
        if desired.met(&before) {
            return Ok(None);
        }
        if !before.installed {
            return Err(unavailable("所选配置里没有安装这个插件。"));
        }
        let binary = match target.client.as_str() {
            "claude" => super::claude_install::binary(ctx)?,
            "codex" if matches!(desired, Desired::Absent) => super::codex_install::binary(ctx)?,
            _ => {
                return Err(unavailable(
                    "该客户端没有已核验的原生启停命令。请用它的插件菜单。",
                ));
            }
        };
        let binary_digest = digest_binary(&binary)?;
        Ok(Some(Self {
            target,
            id: id.into(),
            desired,
            before,
            binary,
            binary_digest,
            market,
        }))
    }

    pub(crate) fn target_version(&self) -> Option<&str> {
        match &self.desired {
            Desired::Version { version, .. } => Some(version),
            _ => None,
        }
    }

    fn reached(&self, current: &Facts) -> bool {
        self.desired.met(current)
            && (!matches!(self.desired, Desired::Enabled(_))
                || (current.version == self.before.version
                    && current.revision == self.before.revision
                    && current.footprint == self.before.footprint
                    && current.payload == self.before.payload
                    && current.origin == self.before.origin))
    }

    fn command(&self) -> CommandSpec {
        let action = match (&self.desired, self.target.client.as_str()) {
            (Desired::Absent, "codex") => "remove",
            (Desired::Absent, _) => "uninstall",
            (Desired::Version { .. }, _) => "update",
            (Desired::Enabled(true), _) => "enable",
            (Desired::Enabled(false), _) => "disable",
        };
        let mut args = vec!["plugin".into(), action.into(), self.id.clone()];
        if self.target.client == "claude" {
            args.extend(["--scope".into(), "user".into()]);
            if action == "uninstall" {
                args.push("--keep-data".into());
            }
        }
        CommandSpec {
            program: self.binary.clone(),
            args,
        }
    }

    fn invocation(&self, home: &Path, timeout: u64) -> FrozenInvocation {
        FrozenInvocation {
            command: self.command(),
            cwd: home.into(),
            timeout_secs: timeout,
            env: BTreeMap::from([
                ("HOME".into(), home.display().to_string()),
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("GIT_TERMINAL_PROMPT".into(), "0".into()),
                ("DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into()),
                (
                    if self.target.client == "claude" {
                        "CLAUDE_CONFIG_DIR"
                    } else {
                        "CODEX_HOME"
                    }
                    .into(),
                    self.target.directory.canonical.display().to_string(),
                ),
            ]),
        }
    }

    pub(crate) fn run(&self, ctx: &Context) -> Result<Value> {
        let store = Store::lock(ctx)?;
        store.check_pending()?;
        let current = read_facts(&self.target, &self.id)?;
        if self.reached(&current) {
            return Ok(json!({"status":"current"}));
        }
        if current != self.before || digest_binary(&self.binary)? != self.binary_digest {
            return Err(changed());
        }
        let home = &self.target.directory.canonical;
        let package = Package {
            id: crate::domain::resource::id(
                "capability",
                &("agent-plugin-action", home, &self.id),
            )?,
            name: self.id.clone(),
            kind: Kind::Plugin,
            official_url: String::new(),
            provider: Provider::Macops,
            paths: vec![],
            exact_version_required: false,
        };
        let hash = fingerprint(self)?;
        let recipe_sha256 = fingerprint(&package)?;
        let plan = Plan {
            brew: None,
            uv_owner: None,
            npm_binding: None,
            timeout_secs: ctx.timeout_secs,
            package,
            action: self.desired.action(),
            before: Observation::Present {
                version: current.version.unwrap_or_else(|| "installed".into()),
                evidence: None,
            },
            target: match &self.desired {
                Desired::Version { version, .. } => Some(version.clone()),
                Desired::Enabled(true) => Some("enabled".into()),
                Desired::Enabled(false) => Some("disabled".into()),
                _ => None,
            },
            command: Some(self.command()),
            query: None,
            recipe_sha256,
            warnings: vec![format!("{MARKER}{hash}")],
            synchronous: true,
            delegated: true,
        };
        let mut journal = crate::operations::new_journal(ctx, plan)?;
        Store::atomic_new(&store.root, &state_name(&journal), self)?;
        store.save(&journal)?;
        let invocation = self.invocation(&ctx.home, ctx.timeout_secs);
        let binding = native_runner::prepare(&store, &journal, &invocation)?;
        let outcome = native_runner::launch(&store, &mut journal, &binding)?;
        finish(&store, &mut journal, self, &outcome)
    }
}

fn state_name(journal: &Journal) -> String {
    format!("agent-plugin-action-{}.json", journal.id)
}

pub(crate) fn owns(journal: &Journal) -> bool {
    journal.plan.delegated
        && journal
            .plan
            .warnings
            .first()
            .is_some_and(|warning| warning.starts_with(MARKER))
}

fn read_intent(journal: &Journal) -> Result<Intent> {
    let path = crate::platform::fs::write_path(&journal.state.join(state_name(journal)))?;
    let data: Intent = crate::platform::fs::regular_json(&path)?;
    if !owns(journal)
        || journal.plan.warnings[0] != format!("{MARKER}{}", fingerprint(&data)?)
        || journal.plan.command.as_ref() != Some(&data.command())
        || journal.plan.action != data.desired.action()
    {
        return Err(changed());
    }
    Ok(data)
}

pub(crate) fn worker_guard(journal: &Journal, invocation: &FrozenInvocation) -> Result<()> {
    let data = read_intent(journal)?;
    if *invocation != data.invocation(&journal.home, journal.plan.timeout_secs)
        || read_facts(&data.target, &data.id)? != data.before
        || digest_binary(&data.binary)? != data.binary_digest
    {
        return Err(changed());
    }
    if let (Some(market), Desired::Version { version, .. }) = (&data.market, &data.desired)
        && pinned_market(&data.target, &data.id, Some(version))? != *market
    {
        return Err(changed());
    }
    Ok(())
}

pub(crate) fn worker_after(
    journal: &Journal,
    _: &FrozenInvocation,
    _: &NativeOutcome,
) -> Result<()> {
    read_intent(journal)?.target.open().map(|_| ())
}

fn finish(
    store: &Store,
    journal: &mut Journal,
    intent: &Intent,
    outcome: &NativeOutcome,
) -> Result<Value> {
    let verified = read_facts(&intent.target, &intent.id);
    let exited = matches!(
        outcome,
        NativeOutcome::Exited {
            code: Some(_),
            signal: None
        }
    );
    let (phase, message) = if exited && verified.as_ref().is_ok_and(|facts| intent.reached(facts)) {
        (Phase::Succeeded, "已核对所选配置的插件状态。")
    } else if exited && verified.as_ref().is_ok_and(|facts| *facts == intent.before) {
        (
            Phase::Failed,
            "原生命令结束，插件保持原状态。请检查该配置的插件菜单。",
        )
    } else {
        (
            Phase::Unknown,
            "还无法核对插件状态。再次操作会先检查原结果。",
        )
    };
    journal.phase = phase;
    journal.message = message.into();
    store.save(journal)?;
    Ok(json!({"journal":journal,"nativeOutcome":outcome}))
}

pub(crate) fn recover(
    store: &Store,
    journal: &mut Journal,
    resolve_failed: bool,
    yes: bool,
) -> Result<Value> {
    let data = read_intent(journal)?;
    if journal.phase == Phase::Prepared && journal.process_group.is_none() {
        return finish(
            store,
            journal,
            &data,
            &NativeOutcome::Exited {
                code: Some(1),
                signal: None,
            },
        );
    }
    if resolve_failed {
        if journal
            .process_group
            .is_some_and(crate::platform::process::group_alive)
        {
            return Err(Error::attention(
                "operation_still_running",
                "先等待上次插件操作结束。",
            ));
        }
        crate::operations::confirm("记录上次插件操作失败。", yes)?;
        journal.phase = Phase::Failed;
        journal.message = "已手动标记上次插件操作失败。".into();
        store.save(journal)?;
        return Ok(json!({"journal":journal}));
    }
    let binding = native_runner::binding(store, journal)?;
    let outcome = native_runner::observe(store, journal, &binding)?;
    finish(store, journal, &data, &outcome)
}
