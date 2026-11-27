use super::{Desired, Facts, InstallTarget, Intent, file, leaf, read_facts, unavailable};
use crate::{
    agent::claude_install_source::{self, Offer},
    capability::{capability_local, capability_store::hash},
    context::Context,
    domain::{FileId, fingerprint},
    error::Result,
    operations::{
        native_runner::{self, NativeOutcome, RunBinding},
        store::Store,
    },
    platform::fs::Directory,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, value::RawValue};
use std::{collections::BTreeMap, ffi::OsStr, os::unix::fs::PermissionsExt as _, path::PathBuf};

const SETTINGS: &str = "settings.json";
const SETTINGS_LIMIT: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GithubMarket {
    pub(super) repo: String,
    pub(super) commit: String,
    files: String,
}

impl GithubMarket {
    fn declaration(&self) -> Value {
        json!({"source":"github","repo":self.repo,"ref":self.commit})
    }

    pub(super) fn verify_cache(&self, target: &InstallTarget, id: &str) -> Result<bool> {
        let state = Scope::read(target, id)?;
        if !state.bound(self) {
            return Ok(false);
        }
        let Some(cache) = state.checkout else {
            return Ok(false);
        };
        let checkout = Directory::open(&cache)?;
        let (_, market) = leaf(id)?;
        if checkout.path != cache || cache != market_slot(target, market)? {
            return Err(blocked());
        }
        let mut reader = super::Passive::new()?;
        let matched = checkout_at(&mut reader, &cache, &self.commit)?
            && fingerprint(&capability_local::claude_market_files(&checkout)?)? == self.files;
        reader.verify()?;
        checkout.verify()?;
        Ok(matched)
    }

    pub(super) fn installed(&self, target: &InstallTarget, id: &str) -> Result<bool> {
        let state = Scope::read(target, id)?;
        if !state.bound(self) {
            return Ok(false);
        }
        let payload = Directory::open(&state.payload)?;
        if payload.path != state.payload {
            return Err(blocked());
        }
        let files = capability_local::claude_updated_files(&payload)?;
        Ok(fingerprint(&files)? == self.files)
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsStamp {
    hash: String,
    identity: FileId,
    mode: u32,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Rebind {
    pub(super) wanted: GithubMarket,
    old: PathBuf,
    stamp: SettingsStamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<Value>,
}

impl Rebind {
    pub(super) fn apply(&self, target: &InstallTarget, id: &str) -> Result<()> {
        let mut state = Scope::read(target, id)?;
        state.verify_old(target, id, &self.old, &self.wanted, self.previous.as_ref())?;
        if state.stamp != self.stamp {
            return Err(blocked());
        }
        if state.declared == self.wanted.declaration() {
            return Ok(());
        }
        let (_, market) = leaf(id)?;
        change_source(&mut state.settings, market, &self.wanted.declaration())?;
        state.root.atomic_replace_regular(
            OsStr::new(SETTINGS),
            &state.settings,
            self.stamp.mode,
            |_| {
                let current = Scope::read(target, id)?;
                current.verify_old(target, id, &self.old, &self.wanted, self.previous.as_ref())?;
                if current.stamp != self.stamp {
                    return Err(blocked());
                }
                Ok(())
            },
        )
    }
}

fn blocked() -> crate::error::Error {
    unavailable(
        "请核对这套 Claude 配置的市场来源。换源需要同名插件、唯一的用户级安装以及可核验的市场记录。",
    )
}

fn check_market(message: &str) -> crate::error::Error {
    unavailable(format!(
        "{message} 打开这套 Claude Code 的 /plugin，进入 Marketplaces 核对来源后重试。"
    ))
}

struct Scope {
    root: Directory,
    settings: Vec<u8>,
    stamp: SettingsStamp,
    declared: Value,
    registered: Value,
    checkout: Option<PathBuf>,
    payload: PathBuf,
}

impl Scope {
    fn read(target: &InstallTarget, id: &str) -> Result<Self> {
        let (plugin, market) = leaf(id)?;
        if target.client != "claude" {
            return Err(blocked());
        }
        let root = target.open()?;
        let mut reader = super::Passive::new()?;
        let (settings, meta) =
            root.regular_config_snapshot(OsStr::new(SETTINGS), SETTINGS_LIMIT)?;
        let Some(also) = file(&mut reader, &root.path.join(SETTINGS))? else {
            return Err(blocked());
        };
        if also != settings {
            return Err(blocked());
        }
        let config = crate::policy::json::parse(&settings)?;
        let declared = config["extraKnownMarketplaces"][market]["source"].clone();
        if !declared.is_object()
            || config["enabledPlugins"].as_object().is_some_and(|enabled| {
                enabled.keys().any(|member| {
                    member != id
                        && member
                            .rsplit_once('@')
                            .is_some_and(|(_, source)| source == market)
                })
            })
        {
            return Err(blocked());
        }
        let markets = file(
            &mut reader,
            &root.path.join("plugins/known_marketplaces.json"),
        )?
        .ok_or_else(blocked)?;
        let markets = crate::policy::json::parse(&markets)?;
        let registered = markets[market]["source"].clone();
        if !registered.is_object() {
            return Err(blocked());
        }
        let checkout = markets[market]["installLocation"]
            .as_str()
            .map(PathBuf::from);
        let book = file(
            &mut reader,
            &root.path.join("plugins/installed_plugins.json"),
        )?
        .ok_or_else(blocked)?;
        let book = crate::policy::catalog::resources::plugin_book(&book)?;
        let suffix = format!("@{market}");
        if book
            .plugins
            .keys()
            .any(|member| member != id && member.ends_with(&suffix))
        {
            return Err(blocked());
        }
        let [record] = book.plugins.get(id).map_or(&[][..], Vec::as_slice) else {
            return Err(blocked());
        };
        let cache = root.path.join("plugins/cache").join(market).join(plugin);
        if record.scope != "user"
            || record.project.is_some()
            || !record.path.starts_with(&cache)
            || record.path == cache
        {
            return Err(blocked());
        }
        reader.verify()?;
        root.verify()?;
        let stamp = SettingsStamp {
            hash: hash(&settings),
            identity: meta.identity,
            mode: meta.mode,
        };
        Ok(Self {
            root,
            settings,
            stamp,
            declared,
            registered,
            checkout,
            payload: record.path.clone(),
        })
    }

    fn bound(&self, wanted: &GithubMarket) -> bool {
        let desired = wanted.declaration();
        self.declared == desired && self.registered == desired
    }

    fn verify_old(
        &self,
        target: &InstallTarget,
        id: &str,
        old: &std::path::Path,
        wanted: &GithubMarket,
        previous: Option<&Value>,
    ) -> Result<()> {
        let directory = json!({"source":"directory","path":old});
        let expected = previous.unwrap_or(&directory);
        if &self.registered != expected
            || (&self.declared != expected && self.declared != wanted.declaration())
            || self.checkout.as_deref() != Some(old)
        {
            return Err(check_market("已登记的来源、配置声明或市场目录不一致。"));
        }
        let (_, market) = leaf(id)?;
        let destination = market_slot(target, market)?;
        if Directory::open(old)?.path != old {
            return Err(blocked());
        }
        let mut reader = super::Passive::new()?;
        if let Some(previous) = previous {
            let commit = github_pin(previous, wanted)?;
            if old != destination || !checkout_at(&mut reader, old, commit)? {
                return Err(check_market("本机市场缓存的位置或提交与登记不一致。"));
            }
        } else if old.starts_with(&destination)
            || destination.starts_with(old)
            || !destination
                .symlink_metadata()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            return Err(blocked());
        }
        let catalog =
            file(&mut reader, &old.join(".claude-plugin/marketplace.json"))?.ok_or_else(blocked)?;
        let catalog = crate::policy::json::parse(&catalog)?;
        let member = member_root(old, &catalog)?;
        let manifest =
            file(&mut reader, &member.join(".claude-plugin/plugin.json"))?.ok_or_else(blocked)?;
        let manifest = crate::policy::json::parse(&manifest)?;
        single_market(&catalog, &manifest, market, id)?;
        reader.verify()
    }
}

fn checkout_at(reader: &mut super::Passive, path: &std::path::Path, commit: &str) -> Result<bool> {
    let head = file(reader, &path.join(".git/HEAD"))?;
    Ok(head
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .map(str::trim)
        == Some(commit))
}

fn github_pin<'a>(source: &'a Value, wanted: &GithubMarket) -> Result<&'a str> {
    use crate::policy::source_git::{ObjectId, Repository};
    let repo = source["repo"]
        .as_str()
        .ok_or_else(|| check_market("市场不是受支持的 GitHub 来源。"))?;
    if source["source"] != "github" || Repository::parse(repo)? != Repository::parse(&wanted.repo)?
    {
        return Err(check_market("已登记市场来自另一个仓库。"));
    }
    let commit = source["ref"]
        .as_str()
        .filter(|value| ObjectId::parse(value).is_ok())
        .ok_or_else(|| check_market("旧市场没有固定到完整的提交。"))?;
    if *source != json!({"source":"github","repo":repo,"ref":commit}) {
        return Err(check_market("旧市场还有其他来源选项，无法确认影响范围。"));
    }
    Ok(commit)
}

fn market_slot(target: &InstallTarget, name: &str) -> Result<PathBuf> {
    crate::platform::fs::write_path(
        &target
            .directory
            .canonical
            .join("plugins/marketplaces")
            .join(name),
    )
}

fn member_root(old: &std::path::Path, catalog: &Value) -> Result<PathBuf> {
    let [member] = catalog["plugins"].as_array().map_or(&[][..], Vec::as_slice) else {
        return Err(blocked());
    };
    let source = member["source"].as_str().ok_or_else(blocked)?;
    let member = if matches!(source, "." | "./") {
        old.to_path_buf()
    } else {
        let path = source.strip_prefix("./").ok_or_else(blocked)?;
        crate::policy::source_git::RelativePath::parse(path)?;
        old.join(path)
    };
    if Directory::open(&member)?.path != member {
        return Err(blocked());
    }
    Ok(member)
}

fn single_market(catalog: &Value, manifest: &Value, market: &str, id: &str) -> Result<()> {
    let (name, _) = leaf(id)?;
    let [member] = catalog["plugins"].as_array().map_or(&[][..], Vec::as_slice) else {
        return Err(blocked());
    };
    let contains_dependencies = |value: &Value| {
        value
            .get("dependencies")
            .is_some_and(|items| items.as_array().is_none_or(|list| !list.is_empty()))
    };
    if catalog["name"] != market
        || manifest["name"] != name
        || member["name"] != name
        || member["source"].as_str().is_none()
        || contains_dependencies(catalog)
        || contains_dependencies(member)
        || contains_dependencies(manifest)
    {
        return Err(blocked());
    }
    Ok(())
}

fn change_source(settings: &mut Vec<u8>, market: &str, next: &Value) -> Result<()> {
    #[derive(Deserialize)]
    struct Settings<'a> {
        #[serde(rename = "extraKnownMarketplaces", borrow)]
        markets: BTreeMap<String, Entry<'a>>,
    }
    #[derive(Deserialize)]
    struct Entry<'a> {
        #[serde(borrow)]
        source: &'a RawValue,
    }
    let scope: Settings<'_> = serde_json::from_slice(settings)?;
    let raw = scope.markets.get(market).ok_or_else(blocked)?.source.get();
    let offset = raw.as_ptr() as usize - settings.as_ptr() as usize;
    let end = offset + raw.len();
    settings.splice(offset..end, serde_json::to_vec(next)?);
    Ok(())
}

pub(crate) struct SwitchOffer {
    offer: Offer,
    market: GithubMarket,
    binary: PathBuf,
    binary_digest: String,
}

impl SwitchOffer {
    pub(crate) fn prepare(offer: &Offer, binary: &std::path::Path) -> Result<Self> {
        let scratch = tempfile::tempdir()?;
        std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o700))?;
        let (slot, _, files) =
            claude_install_source::checkout(offer, &Directory::open(scratch.path())?)?;
        let checkout = Directory::open(&slot.parent.canonical.join(&slot.name))?;
        Self::from_checkout(offer, binary, &checkout, files)
    }

    fn from_checkout(
        offer: &Offer,
        binary: &std::path::Path,
        checkout: &Directory,
        mut files: Vec<(String, bool, String)>,
    ) -> Result<Self> {
        let parse = |name: &str| {
            let bytes = crate::platform::fs::regular_bytes(&checkout.path.join(name))?;
            crate::policy::json::parse(&bytes)
        };
        single_market(
            &parse(claude_install_source::MARKET)?,
            &parse(claude_install_source::PLUGIN)?,
            &offer.marketplace,
            &offer.native_id(),
        )?;
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let market = GithubMarket {
            repo: offer.repository.clone(),
            commit: offer.commit.clone(),
            files: fingerprint(&files)?,
        };
        Ok(Self {
            offer: offer.clone(),
            market,
            binary: binary.to_path_buf(),
            binary_digest: super::digest_binary(binary)?,
        })
    }
}

pub(crate) struct SourceSwitch {
    import: Option<Intent>,
    wanted: GithubMarket,
    target: InstallTarget,
    id: String,
    before: Facts,
    version: String,
    binary: PathBuf,
    binary_digest: String,
    old: Value,
}

impl SourceSwitch {
    pub(crate) fn is_bound(target: &InstallTarget, offer: &Offer) -> Result<bool> {
        let wanted = json!({"source":"github","repo":offer.repository,"ref":offer.commit});
        let state = Scope::read(target, &offer.native_id())?;
        Ok(state.declared == wanted && state.registered == wanted)
    }

    pub(crate) fn plan(
        target: InstallTarget,
        basis: &SwitchOffer,
        allow_change: bool,
    ) -> Result<Option<Self>> {
        let id = basis.offer.native_id();
        let before = read_facts(&target, &id)?;
        let state = Scope::read(&target, &id)?;
        if !before.installed || before.enabled.is_none() {
            return Err(blocked());
        }
        let import = if state.bound(&basis.market) {
            if !basis.market.verify_cache(&target, &id)? {
                return Err(blocked());
            }
            if before.version.as_deref() == Some(&basis.offer.version)
                && basis.market.installed(&target, &id)?
            {
                return Ok(None);
            }
            None
        } else {
            if !allow_change {
                return Err(unavailable(
                    "这套 Claude 配置的来源已改变。请重新核对换源计划。",
                ));
            }
            let old = state.checkout.clone().ok_or_else(blocked)?;
            let previous =
                (state.registered["source"] != "directory").then(|| state.registered.clone());
            state.verify_old(&target, &id, &old, &basis.market, previous.as_ref())?;
            Some(Intent {
                target: target.clone(),
                id: id.clone(),
                desired: Desired::Rebind(Rebind {
                    old,
                    wanted: basis.market.clone(),
                    stamp: state.stamp,
                    previous,
                }),
                before: before.clone(),
                binary: basis.binary.clone(),
                binary_digest: basis.binary_digest.clone(),
                market: None,
            })
        };
        Ok(Some(Self {
            import,
            wanted: basis.market.clone(),
            target,
            id,
            before,
            version: basis.offer.version.clone(),
            binary: basis.binary.clone(),
            binary_digest: basis.binary_digest.clone(),
            old: state.registered,
        }))
    }

    pub(crate) fn version(&self) -> &str {
        &self.version
    }
    pub(crate) fn key(&self) -> (u64, u64, String) {
        let identity = &self.target.directory.identity;
        (
            identity.device,
            identity.inode,
            format!("switch:{}:{}", self.id, self.wanted.commit),
        )
    }
    pub(crate) fn detail(&self) -> String {
        let commit = self.wanted.commit.get(..7).unwrap_or(&self.wanted.commit);
        if !self.changes_source() {
            return format!(
                "市场已固定到 {} 的提交 {commit}，可直接升级插件。",
                self.wanted.repo
            );
        }
        let old = self.old["path"]
            .as_str()
            .or_else(|| self.old["repo"].as_str())
            .unwrap_or("本机市场");
        if let Some(previous) = self.old["ref"].as_str() {
            format!(
                "市场提交 {} → {}。",
                previous.get(..7).unwrap_or(previous),
                commit
            )
        } else {
            format!(
                "换源并升级。市场从 {} 改用 {} 的已核验提交 {}。",
                old, self.wanted.repo, commit
            )
        }
    }

    pub(crate) fn changes_source(&self) -> bool {
        self.import.is_some()
    }

    pub(crate) fn confirmation(&self) -> (String, Vec<String>) {
        let scope = self.target.directory.canonical.display().to_string();
        let scope = Some(("CLAUDE_CONFIG_DIR", scope.as_str()));
        let commands = self
            .import
            .iter()
            .map(Intent::command)
            .chain([self.update_intent(self.before.clone()).command()])
            .map(|command| crate::terminal::plan::native_command(&command, scope))
            .collect();
        (
            format!(
                "{} · 目标版本 {}\n{}\n确认后先更新这套配置的市场声明。",
                self.target.name,
                self.version,
                self.detail()
            ),
            commands,
        )
    }

    fn update_intent(&self, before: Facts) -> Intent {
        Intent {
            target: self.target.clone(),
            id: self.id.clone(),
            desired: Desired::PinnedVersion {
                wanted: self.wanted.clone(),
                version: self.version.clone(),
                enabled: before.enabled,
            },
            before,
            binary: self.binary.clone(),
            binary_digest: self.binary_digest.clone(),
            market: None,
        }
    }

    pub(crate) fn run(self, ctx: &Context) -> Result<Value> {
        self.run_with(ctx, native_runner::launch)
    }

    fn run_with(
        self,
        ctx: &Context,
        mut launch: impl FnMut(
            &Store,
            &mut crate::domain::Journal,
            &RunBinding,
        ) -> Result<NativeOutcome>,
    ) -> Result<Value> {
        let store = Store::lock(ctx)?;
        store.check_pending()?;
        if let Some(import) = &self.import {
            let result = import.run_locked_with(ctx, &store, &mut launch)?;
            if result["journal"]["phase"] != "succeeded" && result["status"] != "current" {
                return Ok(result);
            }
        }
        let current = read_facts(&self.target, &self.id)?;
        if !self.wanted.verify_cache(&self.target, &self.id)? || !self.before.same_install(&current)
        {
            return Err(blocked());
        }
        self.update_intent(current)
            .run_locked_with(ctx, &store, launch)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/plugin_source_switch.rs"]
mod tests;
