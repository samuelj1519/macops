#[path = "native_skills_journey.rs"]
mod journey;

use crate::capability::scoped_files;
use crate::{
    capability::capability_local,
    cli::{Command, SkillCmd},
    context::Context,
    domain::{
        self, Action, CapabilityKind, Journal, Observation, Package, Phase, Plan, Provider,
        QuerySpec, Receipt, Receipts, fingerprint,
    },
    error::{Error, Result},
    operations::{
        native_runner::{self, FrozenInvocation, NativeOutcome},
        store::Store,
    },
    skills::{
        native_skills_assets::{self, Prepared},
        native_skills_files::{self as disk, Snapshot},
        native_skills_steps::{self, Customization},
        rules::native_skills::{self as policy, Book, Recipe},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
};

fn required() -> Error {
    Error::attention(
        "native_skills_recipe_required",
        "本机还没有接管这套技能。运行 mo skill adopt NAME，NAME 换成技能名称。",
    )
}
fn invalid() -> Error {
    Error::attention(
        "native_skills_evidence_changed",
        "固定配方、操作或当前 Native 安装身份已改变，保留 Unknown",
    )
}
pub(crate) fn book_path(ctx: &Context) -> PathBuf {
    ctx.config.join(policy::RECIPE_BOOK_NAME)
}
fn receipt_path(ctx: &Context, id: &str) -> PathBuf {
    ctx.state
        .join("native-skills/receipts")
        .join(format!("{}.json", id.replace(':', "-")))
}
fn frozen_path(journal: &Journal) -> PathBuf {
    journal
        .state
        .join(format!("native-skills-{}-plan.json", journal.id))
}
fn progress_path(journal: &Journal) -> PathBuf {
    journal
        .state
        .join(format!("native-skills-{}-progress.json", journal.id))
}
fn load_book(ctx: &Context) -> Result<(Option<Vec<u8>>, Book)> {
    let raw = scoped_files::regular(&book_path(ctx))?;
    let book = policy::book(raw.as_deref())?;
    Ok((raw, book))
}

pub(crate) fn recommended_recipes(ctx: &Context, subject: &str) -> Result<Vec<Recipe>> {
    let (_, book) = load_book(ctx)?;
    if book.recipes.is_empty() {
        return Ok(vec![]);
    }
    let installed = policy::registry(scoped_files::regular(&disk::lock_path(ctx))?.as_deref())?;
    book.recipes
        .into_iter()
        .filter(|recipe| {
            !installed.contains_key(&recipe.install_name)
                && [
                    &recipe.id,
                    &recipe.install_name,
                    &recipe.source.member_name,
                    &recipe.source.repository,
                ]
                .into_iter()
                .any(|name| name.eq_ignore_ascii_case(subject))
        })
        .filter_map(|recipe| {
            match disk::payload(ctx, &recipe.install_name)
                .and_then(|target| crate::platform::fs::read_path(&target))
            {
                Ok(path) if path.exists() => None,
                Ok(_) => Some(Ok(recipe)),
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}

pub(crate) fn install_recommended(ctx: &Context, recipe: Recipe) -> Result<Value> {
    install_recommended_using(ctx, recipe, crate::operations::confirm)
}

fn install_recommended_using(
    ctx: &Context,
    recipe: Recipe,
    confirm: impl FnOnce(&str, bool) -> Result<()>,
) -> Result<Value> {
    native_config_scope(ctx)?;
    let scope = format!(
        "安装并纳入管理 {} · Skills CLI\n来源 https://github.com/{} · {}\n目标 Codex 用户级 · 仅手动触发 {}\n安装到共享技能目录，其他支持的 Agent 也可能发现",
        recipe.install_name,
        recipe.source.repository,
        recipe.source.commit,
        if recipe.manual_only { "是" } else { "否" }
    );
    confirm(&scope, false)?;
    if let Some(previous) = crate::operations::recover_before_write(ctx, None)?
        && previous.succeeded
        && previous.original.plan.action == Action::Install
        && matches_recommendation(&previous.original, &recipe)?
    {
        return Ok(previous.result);
    }
    let (_, book) = load_book(ctx)?;
    verify_registered_recipe(&book, &recipe)?;
    execute_with_origin(
        ctx,
        recipe,
        Action::Install,
        false,
        false,
        InputFence::default(),
        RecipeOrigin::Registered,
    )
}

fn verify_registered_recipe(book: &Book, recipe: &Recipe) -> Result<()> {
    if !book.recipes.iter().any(|confirmed| confirmed == recipe) {
        return Err(Error::attention(
            "plan_stale",
            "已选择的 Skills CLI 配方发生变化",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecipeOrigin {
    Explicit,
    Registered,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct InputFence {
    pub files: Vec<crate::capability::rules::capability_tx::Witness>,
}
impl InputFence {
    fn verify(&self) -> Result<()> {
        for witness in &self.files {
            if crate::capability::capability_store::observe(&witness.namespace, 16 * 1024 * 1024)?.0
                != witness.state
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
pub fn is_native(journal: &Journal) -> bool {
    journal.plan.delegated
        && journal
            .plan
            .warnings
            .first()
            .is_some_and(|s| s.starts_with("native-skills-v1:"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frozen {
    action: Action,
    package_sha256: String,
    recipe: Recipe,
    snapshot: Snapshot,
    assets: Prepared,
    runner_stamp: String,
    config: PathBuf,
    data: PathBuf,
    config_before: Option<Vec<u8>>,
    config_after: Vec<u8>,
    receipts_before: Option<Vec<u8>>,
    previous_keys: Vec<String>,
    state_binding: crate::policy::governance::DirectoryBinding,
    config_witness: crate::capability::rules::capability_tx::Witness,
    receipts_witness: crate::capability::rules::capability_tx::Witness,
    step_witness: crate::capability::rules::capability_tx::Witness,
    input_fence: InputFence,
    harness_claude: Option<PathBuf>,
    harness_codex: Option<PathBuf>,
    environment: Option<EnvironmentGuards>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentGuards {
    temp: crate::policy::governance::DirectoryBinding,
    gitconfig: crate::capability::rules::capability_tx::Witness,
}
impl EnvironmentGuards {
    fn capture(ctx: &Context) -> Result<Self> {
        let env = disk::frozen_environment(ctx)?;
        let path = Path::new(env.get("TMPDIR").ok_or_else(invalid)?);
        let temp = crate::capability::capability_store::binding(
            &crate::platform::fs::Directory::open(path)?,
            path,
        )?;
        let (gitconfig, bytes) =
            disk::witness(Path::new(env.get("GIT_CONFIG_GLOBAL").ok_or_else(invalid)?))?;
        if bytes.as_deref() != Some(b"") {
            return Err(invalid());
        }
        Ok(Self { temp, gitconfig })
    }
    fn verify(&self) -> Result<()> {
        crate::capability::capability_store::open(
            &crate::capability::rules::capability_tx::Namespace {
                ancestor: self.temp.clone(),
                remaining: vec![".probe".into()],
            },
        )?;
        if crate::capability::capability_store::observe(&self.gitconfig.namespace, 1024)?.0
            != self.gitconfig.state
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Progress {
    stage: String,
    rows_after: Option<BTreeMap<String, Value>>,
    customization: Option<Customization>,
    native_after: Option<crate::skills::native_skills_files::Object>,
    #[serde(default)]
    commits: Vec<disk::Commit>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AfterProof {
    operation_id: String,
    plan_sha256: String,
    worker_pid: u32,
    rows: BTreeMap<String, Value>,
    native: disk::Object,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StepReceipt {
    schema_version: u32,
    installation_id: String,
    operation_id: String,
    recipe_hash: String,
    stage: String,
    action: Action,
    observation_hash: Option<String>,
    native_rows_hash: String,
    native_source: Provider,
    upstream: policy::Source,
}
fn recipe_summary(recipe: &Recipe) -> Result<Value> {
    Ok(
        json!({"id":recipe.id,"recipeHash":policy::recipe_hash(recipe)?,"owner":recipe.owner,
        "cliVersion":recipe.cli_version,"source":recipe.source,"installName":recipe.install_name,
        "scope":recipe.scope,"clientScope":recipe.client_scope,"copy":recipe.copy,"manualOnly":recipe.manual_only,
        "licenseFiles":recipe.license_files,"derivedArtifact":recipe.derived.is_some()}),
    )
}
fn after_name(journal: &Journal) -> String {
    format!("native-skills-{}-after.json", journal.id)
}
fn validate_after(journal: &Journal, progress: &Progress) -> Result<()> {
    let raw =
        scoped_files::regular(&journal.state.join(after_name(journal)))?.ok_or_else(invalid)?;
    let proof: AfterProof = policy::decode(&raw)?;
    if proof.operation_id != journal.id
        || proof.plan_sha256 != journal.plan_sha256
        || Some(proof.worker_pid) != journal.process_group
        || progress.rows_after.as_ref() != Some(&proof.rows)
        || progress.native_after.as_ref() != Some(&proof.native)
    {
        return Err(invalid());
    }
    Ok(())
}
fn frozen(journal: &Journal) -> Result<Frozen> {
    let raw = scoped_files::regular(&frozen_path(journal))?.ok_or_else(invalid)?;
    let frozen: Frozen = policy::decode(&raw)?;
    if journal.plan.warnings.first() != Some(&format!("native-skills-v1:{}", fingerprint(&frozen)?))
    {
        return Err(invalid());
    }
    policy::validate(&frozen.recipe)?;
    if frozen.action != journal.plan.action
        || frozen.package_sha256 != fingerprint(&journal.plan.package)?
    {
        return Err(invalid());
    }
    Ok(frozen)
}
fn context(journal: &Journal, frozen: &Frozen) -> Context {
    Context {
        home: journal.home.clone(),
        scope: journal.scope.clone(),
        state: journal.state.clone(),
        cache: journal.cache.clone(),
        settings: journal.settings.clone(),
        config: frozen.config.clone(),
        data: frozen.data.clone(),
        catalog: frozen.config.join("catalog.json"),
        catalog_explicit: false,
        timeout_secs: journal.plan.timeout_secs,
        isolated: true,
        harness: crate::context::HarnessRoots {
            claude: frozen.harness_claude.clone(),
            codex: frozen.harness_codex.clone(),
        },
        uv: crate::software::uv_local::DiscoveryInputs::default(),
        chezmoi: crate::config::config_inventory::Inputs {
            source: None,
            destination: None,
            config: None,
            env: BTreeMap::new(),
        },
    }
}
fn package(
    ctx: &Context,
    recipe: &Recipe,
    row: Option<&Value>,
    local: Option<&Path>,
) -> Result<Package> {
    let payload = disk::payload(ctx, &recipe.install_name)?;
    let (kind, source) = row.map_or_else(
        || {
            (
                if local.is_some() { "local" } else { "github" }.to_owned(),
                local.map_or_else(
                    || recipe.source.repository.clone(),
                    |p| p.display().to_string(),
                ),
            )
        },
        |row| {
            (
                row["sourceType"].as_str().unwrap_or_default().into(),
                row["source"].as_str().unwrap_or_default().into(),
            )
        },
    );
    Ok(Package {
        id: policy::installation_id(&crate::platform::fs::read_path(&payload)?)?,
        name: recipe.install_name.clone(),
        kind: domain::Kind::Skill,
        official_url: "https://github.com/vercel-labs/skills".into(),
        provider: Provider::CapabilityObserved {
            capability: CapabilityKind::SkillsCli,
            native_id: recipe.install_name.clone(),
            manager: format!("skills-cli:{kind}"),
            record: disk::lock_path(ctx),
            payload: payload.clone(),
            source: Some(source),
            profile_id: None,
            settings: None,
        },
        paths: vec![domain::OwnedPath {
            path: payload,
            kind: domain::PathKind::Payload,
        }],
        exact_version_required: false,
    })
}
fn observation(package: &Package) -> Result<(QuerySpec, Observation)> {
    let query = capability_local::capture(package)?;
    let result = capability_local::observe_default(&package.id, &query)?;
    Ok((QuerySpec::LocalCapability(query), result))
}
fn owners(
    ctx: &Context,
    id: &str,
    action: Action,
) -> Result<(
    Option<Vec<u8>>,
    Vec<String>,
    crate::capability::rules::capability_tx::Witness,
)> {
    let (witness, raw) = disk::witness(&ctx.state.join("receipts.json"))?;
    let book: Receipts = raw
        .as_deref()
        .map(policy::decode)
        .transpose()?
        .unwrap_or(Receipts {
            schema_version: 1,
            installations: BTreeMap::new(),
        });
    crate::policy::operation::state::receipt_schema(&book)?;
    let mut keys = vec![];
    for (key, receipt) in book.installations {
        if matches!(
            receipt.package.provider,
            Provider::CapabilityObserved {
                capability: CapabilityKind::SkillsCli,
                ..
            }
        ) && capability_local::canonical_identity(&receipt.package)? == id
        {
            if receipt
                .scope
                .as_ref()
                .map(policy::native_scope)
                .transpose()?
                .as_deref()
                != Some(policy::native_scope(&ctx.scope)?.as_str())
            {
                return Err(invalid());
            }
            if !matches!(action, Action::Adopt | Action::Install) {
                let (_, current) = observation(&receipt.package)?;
                disk::same(&current, &receipt.observation)?;
            }
            keys.push(key);
        }
    }
    if keys.len() > 1 {
        return Err(Error::attention(
            "owner_conflict",
            "同一中央安装存在多个活动 owner，拒绝写维护",
        ));
    }
    if matches!(action, Action::Upgrade | Action::Uninstall) && keys.is_empty() {
        return Err(Error::attention(
            "adoption_required",
            "先显式采纳当前 Native 安装与固定配方",
        ));
    }
    Ok((raw, keys, witness))
}
pub fn worker_guard(journal: &Journal, invocation: &FrozenInvocation) -> Result<()> {
    if !is_native(journal) {
        return Err(invalid());
    }
    let frozen = frozen(journal)?;
    let ctx = context(journal, &frozen);
    frozen_contract(journal, &frozen, invocation)?;
    original_state(&frozen)?;
    frozen.input_fence.verify()?;
    if disk::runner_stamp(&frozen.recipe)? != frozen.runner_stamp {
        return Err(invalid());
    }
    frozen.assets.verify()?;
    let mut query = std::process::Command::new(&invocation.command.program);
    query
        .arg("--version")
        .env_clear()
        .envs(&invocation.env)
        .current_dir(&invocation.cwd)
        .stdin(std::process::Stdio::null());
    query.process_group(0);
    let output = owned_query(&mut query, &frozen)?;
    if output.code != Some(0) || output.uncertain {
        return Err(invalid());
    }
    policy::node_version(std::str::from_utf8(&output.stdout).map_err(|_| invalid())?)?;
    let mut query = std::process::Command::new(&invocation.command.program);
    query
        .args([
            frozen.recipe.runner.entry.as_os_str(),
            std::ffi::OsStr::new("--version"),
        ])
        .env_clear()
        .envs(&invocation.env)
        .current_dir(&invocation.cwd)
        .stdin(std::process::Stdio::null())
        .process_group(0);
    let output = owned_query(&mut query, &frozen)?;
    if output.code != Some(0)
        || output.uncertain
        || std::str::from_utf8(&output.stdout)
            .map_err(|_| invalid())?
            .trim()
            != "1.9.0"
    {
        return Err(Error::attention(
            "native_skills_cli_version_unsupported",
            "仅执行已冻结的 Skills 1.9.0，不隐式升级工具",
        ));
    }
    frozen.snapshot.before(&ctx, &frozen.recipe)?;
    if scoped_files::regular(&ctx.state.join("native-skills-empty-gitconfig"))?
        .is_none_or(|b| !b.is_empty())
    {
        return Err(invalid());
    }
    if scoped_files::regular(&book_path(&ctx))? != frozen.config_before
        || scoped_files::regular(&ctx.state.join("receipts.json"))? != frozen.receipts_before
    {
        return Err(invalid());
    }
    frozen.input_fence.verify()?;
    Ok(())
}
fn frozen_contract(
    journal: &Journal,
    frozen: &Frozen,
    invocation: &FrozenInvocation,
) -> Result<()> {
    let ctx = context(journal, frozen);
    original_state(frozen)?;
    frozen.environment.as_ref().ok_or_else(invalid)?.verify()?;
    let expected = package(
        &ctx,
        &frozen.recipe,
        if journal.plan.action == Action::Uninstall {
            frozen.snapshot.rows.get(&frozen.recipe.install_name)
        } else {
            None
        },
        frozen.assets.local.as_deref(),
    )?;
    disk::same(&expected, &journal.plan.package)?;
    if mutation_command(
        &frozen.recipe,
        journal.plan.action,
        frozen.assets.local.as_deref(),
    )?
    .as_ref()
        != Some(&invocation.command)
        || invocation.env != disk::frozen_environment(&ctx)?
        || invocation.cwd != journal.home
        || disk::runner_stamp(&frozen.recipe)? != frozen.runner_stamp
    {
        return Err(invalid());
    }
    frozen.assets.verify()
}
fn owned_query(
    query: &mut std::process::Command,
    frozen: &Frozen,
) -> Result<crate::platform::process::BinaryOutput> {
    let directory = original_state(frozen)?;
    query_capture(query, &directory, || original_state(frozen).map(|_| ()))
}
fn query_capture(
    query: &mut std::process::Command,
    directory: &crate::platform::fs::Directory,
    guard: impl FnMut() -> Result<()>,
) -> Result<crate::platform::process::BinaryOutput> {
    let captured = crate::platform::process::capture_owned_group_in(
        query,
        std::time::Duration::from_secs(10),
        8192,
        &directory.path,
        guard,
    );
    captured.result
}
fn original_state(frozen: &Frozen) -> Result<crate::platform::fs::Directory> {
    crate::capability::capability_store::open(&crate::capability::rules::capability_tx::Namespace {
        ancestor: frozen.state_binding.clone(),
        remaining: vec![".native-probe".into()],
    })
}
pub fn worker_after(
    journal: &Journal,
    invocation: &FrozenInvocation,
    outcome: &NativeOutcome,
) -> Result<()> {
    if *outcome
        != (NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        })
    {
        return Ok(());
    }
    let frozen = frozen(journal)?;
    frozen_contract(journal, &frozen, invocation)?;
    let ctx = context(journal, &frozen);
    let root = original_state(&frozen)?;
    let result: Result<()> = (|| {
        let rows = frozen.snapshot.after(
            &ctx,
            &frozen.recipe,
            frozen.assets.local.as_deref(),
            journal.plan.action == Action::Uninstall,
        )?;
        if journal.plan.action != Action::Uninstall
            && let Some(local) = &frozen.assets.local
        {
            native_skills_assets::installed_derived(&ctx, &frozen.recipe, local)?;
        }
        let native_after = disk::capture(&disk::payload(&ctx, &frozen.recipe.install_name)?)?;
        Store::atomic_new(
            root.file(),
            &after_name(journal),
            &AfterProof {
                operation_id: journal.id.clone(),
                plan_sha256: journal.plan_sha256.clone(),
                worker_pid: std::process::id(),
                rows: rows.clone(),
                native: native_after.clone(),
            },
        )?;
        let progress = Progress {
            stage: "native-after-observed".into(),
            rows_after: Some(rows),
            customization: None,
            native_after: Some(native_after),
            commits: vec![],
        };
        Store::atomic(
            root.file(),
            progress_path(journal)
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(invalid)?,
            &progress,
        )?;
        original_state(&frozen)?;
        Ok(())
    })();
    if let Err(error) = &result {
        let diagnostic = json!({"operationId":journal.id,"planHash":journal.plan_sha256,"workerPid":std::process::id(),"reason":error.code});
        Store::atomic(
            root.file(),
            &format!("native-skills-{}-after-error.json", journal.id),
            &diagnostic,
        )?;
    }
    result
}
fn read_recipe(path: &Path) -> Result<(Recipe, InputFence)> {
    let (witness, raw) = disk::witness(path)?;
    let raw = raw.ok_or_else(|| {
        Error::attention(
            "native_skills_recipe_required",
            format!("本机没有找到配方文件「{}」。", path.display()),
        )
        .with_guidance(
            "无法读取 --recipe 指定的技能配方。",
            "确认配方 JSON 文件存在，把实际绝对路径传给 --recipe，再运行原命令。",
        )
    })?;
    let recipe: Recipe = policy::decode(&raw)?;
    policy::validate(&recipe)?;
    Ok((
        recipe,
        InputFence {
            files: vec![witness],
        },
    ))
}
fn check_installed_recipe(
    ctx: &Context,
    recipe: &Recipe,
    snapshot: &Snapshot,
    action: Action,
) -> Result<()> {
    let row = snapshot
        .rows
        .get(&recipe.install_name)
        .ok_or_else(required)?;
    if !snapshot.selected.present() {
        return Err(invalid());
    }
    if row["sourceType"] == "github" {
        if row["source"].as_str().map(str::to_ascii_lowercase)
            != Some(recipe.source.repository.to_ascii_lowercase())
            || policy::skill_directory(row["skillPath"].as_str().ok_or_else(invalid)?)?
                != recipe.source.skill_directory
        {
            return Err(invalid());
        }
        crate::policy::source_git::Repository::parse(&recipe.source.repository)?
            .verify_url(row["sourceUrl"].as_str().ok_or_else(invalid)?)?;
        if action == Action::Adopt {
            let commit = row.get("commit").and_then(Value::as_str).or_else(|| {
                row.get("ref")
                    .and_then(Value::as_str)
                    .filter(|v| crate::policy::source_git::ObjectId::parse(v).is_ok())
            });
            if commit.is_some_and(|c| c != recipe.source.commit) {
                return Err(invalid());
            }
        }
    } else if row["sourceType"] != "local" || recipe.derived.is_none() {
        return Err(invalid());
    } else {
        native_skills_assets::owned_local(
            ctx,
            recipe,
            Path::new(row["source"].as_str().ok_or_else(invalid)?),
            action == Action::Adopt,
        )?;
    }
    Ok(())
}
fn seal_runner(recipe: &mut Recipe, action: Action) -> Result<String> {
    let runner_stamp = disk::runner_stamp(recipe)?;
    if let Some(expected) = &recipe.runner.sha256 {
        if *expected != runner_stamp {
            return Err(Error::attention(
                "native_skills_runner_changed",
                "固定 runner 摘要已改变；需要显式新配方接受，不隐式安装或学习工具",
            ));
        }
    } else if !matches!(action, Action::Install | Action::Adopt) {
        return Err(required());
    }
    recipe.runner.sha256 = Some(runner_stamp.clone());
    Ok(runner_stamp)
}

fn prior_step(
    ctx: &Context,
    package: &Package,
    recipe: &Recipe,
    action: Action,
) -> Result<crate::capability::rules::capability_tx::Witness> {
    let (step_witness, step_raw) = disk::witness(&receipt_path(ctx, &package.id))?;
    if let Some(raw) = step_raw {
        let previous: StepReceipt = policy::decode(&raw)?;
        if previous.schema_version != 1 || previous.installation_id != package.id {
            return Err(invalid());
        }
        let original: Journal = crate::platform::fs::regular_json(
            &ctx.state
                .join("operations")
                .join(format!("{}.json", previous.operation_id)),
        )?;
        crate::operations::store::validate_journal(&original)?;
        if original.phase != Phase::Succeeded || original.plan.package.id != package.id {
            return Err(invalid());
        }
        if matches!(action, Action::Upgrade | Action::Uninstall) {
            let raw = scoped_files::regular(
                &ctx.data
                    .join("native-skills/recipes")
                    .join(format!("{}.json", previous.recipe_hash)),
            )?
            .ok_or_else(invalid)?;
            let mut installed: Recipe = policy::decode(&raw)?;
            if policy::recipe_hash(&installed)? != previous.recipe_hash {
                return Err(invalid());
            }
            if action == Action::Upgrade {
                update_expected_version(&mut installed, recipe);
            }
            disk::same(&installed, recipe)?;
        }
    }
    Ok(step_witness)
}

fn update_expected_version(previous: &mut Recipe, next: &Recipe) {
    previous.source.commit.clone_from(&next.source.commit);
    if policy::generated(previous)
        && previous.source.tracking_ref.as_deref() == Some("HEAD")
        && next.source.tracking_ref.is_none()
    {
        previous.source.tracking_ref = None;
    }
}

fn validate_action(
    ctx: &Context,
    recipe: &Recipe,
    action: Action,
    snapshot: &Snapshot,
) -> Result<()> {
    if action == Action::Install {
        if snapshot.selected.present() || snapshot.rows.contains_key(&recipe.install_name) {
            return Err(Error::attention(
                "adoption_required",
                "目标已存在；先显式采纳，不能用安装覆盖未知 owner",
            ));
        }
    } else {
        check_installed_recipe(ctx, recipe, snapshot, action)?;
    }
    if action == Action::Uninstall {
        snapshot.remove_unshared(ctx, recipe)?;
    }
    Ok(())
}

fn plan(
    ctx: &Context,
    recipe: Recipe,
    action: Action,
    input_fence: InputFence,
) -> Result<(Frozen, Plan)> {
    plan_with_origin(ctx, recipe, action, input_fence, RecipeOrigin::Explicit)
}

fn plan_with_origin(
    ctx: &Context,
    mut recipe: Recipe,
    action: Action,
    input_fence: InputFence,
    origin: RecipeOrigin,
) -> Result<(Frozen, Plan)> {
    input_fence.verify()?;
    let snapshot = checked_snapshot(ctx, &recipe)?;
    validate_action(ctx, &recipe, action, &snapshot)?;
    let current = package(ctx, &recipe, snapshot.rows.get(&recipe.install_name), None)?;
    let (receipts_before, previous_keys, receipts_witness) = owners(ctx, &current.id, action)?;
    let before = if snapshot.rows.contains_key(&recipe.install_name) {
        observation(&current)?.1
    } else {
        Observation::CapabilityPack {
            capability: CapabilityKind::SkillsCli,
            pack: None,
        }
    };
    let (config_witness, config_before) = disk::witness(&book_path(ctx))?;
    let previous = policy::book(config_before.as_deref())?;
    if origin == RecipeOrigin::Registered {
        verify_registered_recipe(&previous, &recipe)?;
    }
    let runner_stamp = seal_runner(&mut recipe, action)?;
    if matches!(action, Action::Upgrade | Action::Uninstall) {
        let mut selected = previous
            .recipes
            .iter()
            .find(|r| r.install_name == recipe.install_name)
            .cloned()
            .ok_or_else(required)?;
        if action == Action::Upgrade {
            update_expected_version(&mut selected, &recipe);
        }
        disk::same(&selected, &recipe)?;
    }
    let book = policy::merge(previous, &recipe)?;
    let config_after = disk::encode(&book)?;
    let assets = if matches!(action, Action::Adopt | Action::Uninstall) {
        Prepared {
            local: None,
            local_digest: None,
            licenses: BTreeMap::new(),
            upstream_tree: String::new(),
        }
    } else {
        native_skills_assets::prepare(ctx, &recipe)?
    };
    let package = if action == Action::Adopt || action == Action::Uninstall {
        current
    } else {
        package(ctx, &recipe, None, assets.local.as_deref())?
    };
    let command = mutation_command(&recipe, action, assets.local.as_deref())?;
    let step_witness = prior_step(ctx, &package, &recipe, action)?;
    let frozen = Frozen {
        action,
        package_sha256: fingerprint(&package)?,
        recipe,
        snapshot,
        assets,
        runner_stamp,
        config: ctx.config.clone(),
        data: ctx.data.clone(),
        config_before,
        config_after,
        receipts_before,
        previous_keys,
        state_binding: crate::capability::capability_store::binding(
            &crate::platform::fs::Directory::open(&ctx.state)?,
            &ctx.state,
        )?,
        config_witness,
        receipts_witness,
        step_witness,
        input_fence,
        harness_claude: ctx.harness.claude.clone(),
        harness_codex: ctx.harness.codex.clone(),
        environment: if action == Action::Adopt {
            None
        } else {
            Some(EnvironmentGuards::capture(ctx)?)
        },
    };
    let plan = Plan {
        npm_binding: None,
        uv_owner: None,
        brew: None,
        timeout_secs: ctx.timeout_secs,
        package: package.clone(),
        action,
        before,
        target: None,
        command,
        query: None,
        recipe_sha256: fingerprint(&package)?,
        warnings: vec![format!("native-skills-v1:{}", fingerprint(&frozen)?)],
        synchronous: true,
        delegated: true,
    };
    Ok((frozen, plan))
}

fn mutation_command(
    recipe: &Recipe,
    action: Action,
    local: Option<&Path>,
) -> Result<Option<domain::CommandSpec>> {
    let command = if action == Action::Adopt {
        None
    } else {
        let args = if action == Action::Uninstall {
            vec![
                recipe.runner.entry.display().to_string(),
                "remove".into(),
                "--global".into(),
                "--yes".into(),
                "--agent".into(),
                "codex".into(),
                "--skill".into(),
                recipe.install_name.clone(),
            ]
        } else {
            policy::add_argv(recipe, local)?
        };
        Some(domain::CommandSpec {
            program: recipe.runner.node.clone(),
            args,
        })
    };
    Ok(command)
}
fn save_progress(store: &Store, journal: &Journal, progress: &Progress) -> Result<()> {
    store.verify_namespace()?;
    Store::atomic(
        &store.root,
        progress_path(journal)
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(invalid)?,
        progress,
    )?;
    store.verify_namespace()
}
fn progress(journal: &Journal) -> Result<Progress> {
    policy::decode(&scoped_files::regular(&progress_path(journal))?.ok_or_else(invalid)?)
}
fn record_images(
    ctx: &Context,
    journal: &Journal,
    frozen: &Frozen,
    progress: &Progress,
) -> Result<Vec<(crate::capability::rules::capability_tx::Witness, Vec<u8>)>> {
    let current = if journal.plan.action == Action::Uninstall {
        None
    } else {
        Some(observation(&journal.plan.package)?)
    };
    let mut receipts: Receipts = frozen
        .receipts_before
        .as_deref()
        .map(policy::decode)
        .transpose()?
        .unwrap_or(Receipts {
            schema_version: 1,
            installations: BTreeMap::new(),
        });
    for key in &frozen.previous_keys {
        receipts.installations.remove(key);
    }
    if let Some((query, observation)) = &current {
        receipts.installations.insert(
            journal.plan.package.id.clone(),
            Receipt {
                scope: Some(journal.scope.clone()),
                query: Some(query.clone()),
                package: journal.plan.package.clone(),
                observation: observation.clone(),
                operation_id: journal.id.clone(),
                updated_at: journal.started_at,
            },
        );
    }
    let stage = if journal.plan.action == Action::Adopt {
        "adopted-baseline"
    } else {
        "verified"
    };
    let step = StepReceipt {
        schema_version: 1,
        installation_id: journal.plan.package.id.clone(),
        operation_id: journal.id.clone(),
        recipe_hash: policy::recipe_hash(&frozen.recipe)?,
        stage: stage.into(),
        action: journal.plan.action,
        observation_hash: current.as_ref().map(|(_, o)| fingerprint(o)).transpose()?,
        native_rows_hash: fingerprint(&progress.rows_after)?,
        native_source: journal.plan.package.provider.clone(),
        upstream: frozen.recipe.source.clone(),
    };
    let paths = [
        ctx.state.join("receipts.json"),
        book_path(ctx),
        receipt_path(ctx, &journal.plan.package.id),
    ];
    let witnesses = [
        frozen.receipts_witness.clone(),
        frozen.config_witness.clone(),
        frozen.step_witness.clone(),
    ];
    let images = [
        disk::encode(&receipts)?,
        frozen.config_after.clone(),
        disk::encode(&step)?,
    ];
    for (path, witness) in paths.iter().zip(&witnesses) {
        if *path != witness.path {
            return Err(invalid());
        }
    }
    Ok(witnesses.into_iter().zip(images).collect())
}
fn commit(
    ctx: &Context,
    store: &Store,
    journal: &Journal,
    frozen: &Frozen,
    progress: &mut Progress,
) -> Result<()> {
    let images = record_images(ctx, journal, frozen, progress)?;
    if progress.commits.is_empty() {
        for (witness, bytes) in &images {
            progress
                .commits
                .push(disk::Commit::prepare(witness.clone(), bytes.clone())?);
        }
        save_progress(store, journal, progress)?;
    }
    if progress.commits.len() != images.len() {
        return Err(invalid());
    }
    for (record, (witness, bytes)) in progress.commits.iter().zip(&images) {
        if record.before != *witness {
            return Err(invalid());
        }
        record.publish(&witness.path, bytes, || {
            store.verify_namespace()?;
            if journal.plan.action == Action::Adopt {
                frozen.snapshot.before(ctx, &frozen.recipe)?;
            }
            if let Some(steps) = &progress.customization {
                steps.validate(
                    progress.native_after.as_ref().ok_or_else(invalid)?,
                    &frozen.recipe,
                    &frozen.assets.licenses,
                    frozen.assets.local.is_some(),
                )?;
                if !steps.complete()? {
                    return Err(invalid());
                }
            }
            if journal.plan.action != Action::Adopt {
                let rows = frozen.snapshot.after(
                    ctx,
                    &frozen.recipe,
                    frozen.assets.local.as_deref(),
                    journal.plan.action == Action::Uninstall,
                )?;
                disk::same(progress.rows_after.as_ref().ok_or_else(invalid)?, &rows)?;
            }
            Ok(())
        })?;
    }
    store.verify_namespace()
}

fn finish(ctx: &Context, store: &Store, journal: &mut Journal, frozen: &Frozen) -> Result<Value> {
    let mut progress = progress(journal)?;
    let result = (|| {
        validate_after(journal, &progress)?;
        let remove = journal.plan.action == Action::Uninstall;
        let rows =
            frozen
                .snapshot
                .after(ctx, &frozen.recipe, frozen.assets.local.as_deref(), remove)?;
        if let Some(expected) = &progress.rows_after {
            disk::same(expected, &rows)?;
        } else {
            return Err(invalid());
        }
        if progress.native_after.is_none() {
            return Err(invalid());
        }
        progress.stage = "native-succeeded".into();
        save_progress(store, journal, &progress)?;
        if !remove {
            progress.stage = "customization-pending".into();
            save_progress(store, journal, &progress)?;
            if progress.customization.is_none() {
                progress
                    .native_after
                    .as_ref()
                    .ok_or_else(invalid)?
                    .verify()?;
                progress.customization = Some(native_skills_steps::prepare(
                    &disk::payload(ctx, &frozen.recipe.install_name)?,
                    &frozen.recipe,
                    &frozen.assets.licenses,
                    frozen.assets.local.is_some(),
                )?);
                progress.stage = "customization-pending".into();
                save_progress(store, journal, &progress)?;
            }
            let mut steps = progress.customization.take().ok_or_else(invalid)?;
            steps.validate(
                progress.native_after.as_ref().ok_or_else(invalid)?,
                &frozen.recipe,
                &frozen.assets.licenses,
                frozen.assets.local.is_some(),
            )?;
            for index in 0..steps.patches.len() {
                frozen.snapshot.protected(ctx, &frozen.recipe)?;
                store.verify_namespace()?;
                steps.apply(index, |updated| {
                    let mut saved = progress.clone();
                    saved.customization = Some(updated.clone());
                    save_progress(store, journal, &saved)
                })?;
            }
            if !steps.complete()? {
                return Err(invalid());
            }
            progress.customization = Some(steps);
        }
        frozen
            .snapshot
            .after(ctx, &frozen.recipe, frozen.assets.local.as_deref(), remove)?;
        commit(ctx, store, journal, frozen, &mut progress)?;
        progress.stage = "verified".into();
        save_progress(store, journal, &progress)?;
        journal.phase = Phase::Succeeded;
        journal.message = "Native 结果与固定交付步骤全部核验通过".into();
        store.save(journal)?;
        Ok::<(), Error>(())
    })();
    if let Err(error) = result {
        if matches!(
            progress.stage.as_str(),
            "native-running" | "native-after-observed"
        ) {
            progress.stage = "native-unknown".into();
            save_progress(store, journal, &progress)?;
        }
        journal.phase = Phase::Unknown;
        journal.message = error.code;
        store.save(journal)?;
    }
    Ok(
        json!({"backend":"rust","journal":journal,"deliveryStage":progress.stage,"sharedCanonical":disk::payload(ctx,&frozen.recipe.install_name)?}),
    )
}
fn persist_recipe(ctx: &Context, frozen: &Frozen) -> Result<()> {
    disk::write_new(
        &ctx.data
            .join("native-skills/recipes")
            .join(format!("{}.json", policy::recipe_hash(&frozen.recipe)?)),
        &frozen.recipe,
    )
    .or_else(|error| {
        if error.code == "already_exists" {
            let raw = scoped_files::regular(
                &ctx.data
                    .join("native-skills/recipes")
                    .join(format!("{}.json", policy::recipe_hash(&frozen.recipe)?)),
            )?
            .ok_or_else(invalid)?;
            let previous: Recipe = policy::decode(&raw)?;
            disk::same(&previous, &frozen.recipe)
        } else {
            Err(error)
        }
    })
}

fn execute(
    ctx: &Context,
    recipe: Recipe,
    action: Action,
    dry: bool,
    yes: bool,
    input_fence: InputFence,
) -> Result<Value> {
    execute_with_origin(
        ctx,
        recipe,
        action,
        dry,
        yes,
        input_fence,
        RecipeOrigin::Explicit,
    )
}

fn prepare_execution(
    ctx: &Context,
    store: &Store,
    recipe: Recipe,
    action: Action,
    input_fence: InputFence,
    origin: RecipeOrigin,
) -> Result<(Frozen, Journal)> {
    let (frozen, plan) = if origin == RecipeOrigin::Registered {
        plan_with_origin(ctx, recipe, action, input_fence, origin)?
    } else {
        plan(ctx, recipe, action, input_fence)?
    };
    let journal = crate::operations::new_journal(ctx, plan)?;
    disk::write_new(&frozen_path(&journal), &frozen)?;
    persist_recipe(ctx, &frozen)?;
    store.save(&journal)?;
    Ok((frozen, journal))
}

fn checked_snapshot(ctx: &Context, recipe: &Recipe) -> Result<Snapshot> {
    policy::validate(recipe)?;
    let target = disk::payload(ctx, &recipe.install_name)?;
    let package_root = recipe
        .runner
        .entry
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(invalid)?;
    let protected = [
        &ctx.config,
        &ctx.data,
        &ctx.state,
        &ctx.cache,
        &ctx.settings,
        &ctx.catalog,
        &recipe.runner.node,
    ];
    let mut paths = protected
        .into_iter()
        .map(|p| crate::platform::fs::read_path(p))
        .collect::<Result<Vec<_>>>()?;
    paths.push(crate::platform::fs::read_path(package_root)?);
    paths.push(crate::platform::fs::read_path(&disk::lock_path(ctx))?);
    crate::source::rules::source_baseline::disjoint(
        &crate::platform::fs::read_path(&target)?,
        &paths,
    )?;
    disk::local_scope(ctx, &recipe.install_name, &paths)?;
    disk::snapshot(ctx, recipe)
}

fn execute_with_origin(
    ctx: &Context,
    recipe: Recipe,
    action: Action,
    dry: bool,
    yes: bool,
    input_fence: InputFence,
    origin: RecipeOrigin,
) -> Result<Value> {
    let snapshot = checked_snapshot(ctx, &recipe)?;
    if dry {
        let (_, book) = load_book(ctx)?;
        return Ok(
            json!({"backend":"rust","mode":"dry-run","recipe":recipe,"selectedPresent":snapshot.selected.present(),"configuredRecipes":book.recipes.len(),"resolution":"not-executed","persistentWrites":false,"networkInvoked":false}),
        );
    }
    validate_action(ctx, &recipe, action, &snapshot)?;
    if matches!(action, Action::Adopt | Action::Uninstall) {
        crate::operations::confirm(&format!("{action:?} {}", recipe.install_name), yes)?;
    }
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    scoped_files::private(&ctx.config)?;
    scoped_files::private(&ctx.data)?;
    scoped_files::private(&ctx.state.join("native-skills/receipts"))?;
    let (frozen, mut journal) =
        prepare_execution(ctx, &store, recipe, action, input_fence, origin)?;
    if action == Action::Adopt {
        let mut progress = Progress {
            stage: "adopted-baseline".into(),
            rows_after: Some(frozen.snapshot.rows.clone()),
            customization: None,
            native_after: Some(frozen.snapshot.selected.clone()),
            commits: vec![],
        };
        save_progress(&store, &journal, &progress)?;
        commit(ctx, &store, &journal, &frozen, &mut progress)?;
        journal.phase = Phase::Succeeded;
        journal.message = "只采纳当前 Native 基线；配方定制差异由查询报告，未运行安装器".into();
        store.save(&journal)?;
        return Ok(
            json!({"journal":journal,"deliveryStage":"adopted-baseline","persistentWrites":true,"nativeInvoked":false}),
        );
    }
    save_progress(
        &store,
        &journal,
        &Progress {
            stage: "native-running".into(),
            rows_after: None,
            customization: None,
            native_after: None,
            commits: vec![],
        },
    )?;
    let invocation = FrozenInvocation {
        command: journal.plan.command.clone().ok_or_else(invalid)?,
        env: disk::frozen_environment(ctx)?,
        cwd: ctx.home.clone(),
        timeout_secs: ctx.timeout_secs,
    };
    let binding = native_runner::prepare(&store, &journal, &invocation)?;
    let outcome = native_runner::launch(&store, &mut journal, &binding)?;
    if outcome
        != (NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        })
    {
        journal.phase = Phase::Unknown;
        journal.message = "Native 失败、部分结果或完成证据未知；先查询原操作，不重跑".into();
        store.save(&journal)?;
        return Ok(
            json!({"journal":journal,"nativeOutcome":outcome,"deliveryStage":"native-unknown"}),
        );
    }
    finish(ctx, &store, &mut journal, &frozen)
}
pub fn recover(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    resolve_failed: bool,
    yes: bool,
) -> Result<Value> {
    let frozen = frozen(journal)?;
    if journal.home != ctx.home || journal.state != ctx.state {
        return Err(invalid());
    }
    if matches!(journal.phase, Phase::Succeeded | Phase::Failed) {
        return Ok(json!({"journal":journal}));
    }
    if journal
        .process_group
        .is_some_and(crate::platform::process::group_alive)
    {
        return Err(Error::attention(
            "operation_still_running",
            "原 Native 进程组仍在运行，不能恢复定制或解除阻断",
        ));
    }
    if resolve_failed {
        crate::operations::confirm(
            "人工核对后只记录失败并解除阻断；不重跑 Native、不回滚 Native 根",
            yes,
        )?;
        journal.phase = Phase::Failed;
        store.save(journal)?;
        return Ok(json!({"journal":journal}));
    }
    if journal.plan.action == Action::Adopt {
        let frozen_ctx = context(journal, &frozen);
        frozen.snapshot.before(&frozen_ctx, &frozen.recipe)?;
        let mut progress = progress(journal)?;
        if progress.stage != "adopted-baseline"
            || progress.customization.is_some()
            || progress.rows_after.as_ref() != Some(&frozen.snapshot.rows)
            || progress.native_after.as_ref() != Some(&frozen.snapshot.selected)
        {
            return Err(invalid());
        }
        commit(&frozen_ctx, store, journal, &frozen, &mut progress)?;
        journal.phase = Phase::Succeeded;
        journal.message = "原采纳的自有记录已核验并完成；未启动 Native".into();
        store.save(journal)?;
        return Ok(
            json!({"journal":journal,"deliveryStage":"adopted-baseline","nativeInvoked":false}),
        );
    }
    let binding = native_runner::binding(store, journal)?;
    let outcome = native_runner::observe(store, journal, &binding)?;
    if outcome
        != (NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        })
    {
        return Ok(
            json!({"journal":journal,"nativeOutcome":outcome,"deliveryStage":"native-unknown"}),
        );
    }
    let frozen_ctx = context(journal, &frozen);
    finish(&frozen_ctx, store, journal, &frozen)
}
fn compatibility_keys(
    ctx: &Context,
    recipe: &Recipe,
    row: Option<&Value>,
) -> Result<std::collections::BTreeSet<String>> {
    let mut keys = std::collections::BTreeSet::from([
        recipe.id.clone(),
        recipe.install_name.clone(),
        format!("skill:{}", recipe.install_name),
        format!("asset:{}", recipe.id),
        format!("skill:{}", recipe.source.repository.to_ascii_lowercase()),
    ]);
    keys.insert(policy::installation_id(&crate::platform::fs::read_path(
        &disk::payload(ctx, &recipe.install_name)?,
    )?)?);
    for source in [
        &recipe.source.repository,
        &recipe.source.repository.to_ascii_lowercase(),
    ] {
        keys.insert(
            crate::capability::rules::capability_local::skill_capability_id(
                &disk::lock_path(ctx),
                &recipe.install_name,
                "github",
                source,
            )?,
        );
    }
    if let Some(row) = row {
        keys.insert(
            crate::capability::rules::capability_local::skill_capability_id(
                &disk::lock_path(ctx),
                &recipe.install_name,
                row["sourceType"].as_str().ok_or_else(invalid)?,
                row["source"].as_str().ok_or_else(invalid)?,
            )?,
        );
    }
    Ok(keys)
}
fn select(ctx: &Context, key: &str) -> Result<Recipe> {
    let (_, book) = load_book(ctx)?;
    let rows = policy::registry(scoped_files::regular(&disk::lock_path(ctx))?.as_deref())?;
    select_registered(ctx, key, book, &rows)
}

fn select_registered(
    ctx: &Context,
    key: &str,
    book: Book,
    rows: &BTreeMap<String, Value>,
) -> Result<Recipe> {
    let normalized = repository_selector(key);
    let key = normalized.as_deref().unwrap_or(key);
    let mut found = vec![];
    for recipe in book.recipes {
        if compatibility_keys(ctx, &recipe, rows.get(&recipe.install_name))?.contains(key) {
            found.push(recipe);
        }
    }
    match found.as_slice() {
        [recipe] => Ok(recipe.clone()),
        [] => Err(required()),
        _ => Err(Error::attention("ambiguous", "请使用明确安装 ID")),
    }
}

pub(crate) fn accepts_inventory_maintenance(ctx: &Context, key: &str) -> bool {
    native_config_scope(ctx).is_ok() && select(ctx, key).is_ok()
}

pub(crate) fn maintain_from_inventory(ctx: &Context, key: &str, action: Action) -> Result<Value> {
    inventory_maintenance(ctx, key, action, crate::operations::confirm)
}

pub(crate) fn maintain_shared_from_inventory(
    ctx: &Context,
    id: &str,
    action: Action,
) -> Result<Value> {
    use crate::domain::resource::CapabilityAction;
    let (operation, verb) = match action {
        Action::Upgrade => (CapabilityAction::Upgrade, "升级"),
        Action::Uninstall => (CapabilityAction::Uninstall, "卸载"),
        _ => return Err(invalid()),
    };
    if !id.starts_with("capability:") {
        return Err(invalid());
    }
    let SharedPlan::Maintain(plan) = plan_shared(ctx, id, operation, None)? else {
        return Err(invalid());
    };
    let [(current, _)] = plan.members.as_slice() else {
        return Err(Error::attention(
            "ambiguous",
            "选中的技能对应多份安装。请核对安装位置。",
        ));
    };
    let path = disk::payload(ctx, &current.install_name)?;
    crate::operations::confirm(
        &format!(
            "{verb} {} · Skills CLI\n共享安装 {}\n此改动会影响使用此共享目录的 Agent。",
            current.install_name,
            crate::output::sanitize(&path.display().to_string(), false),
        ),
        false,
    )?;
    crate::operations::recover_before_write(ctx, None)?;
    plan.run(ctx)
}

fn inventory_maintenance(
    ctx: &Context,
    key: &str,
    action: Action,
    confirm: impl FnOnce(&str, bool) -> Result<()>,
) -> Result<Value> {
    native_config_scope(ctx)?;
    let label = match action {
        Action::Upgrade => "升级",
        Action::Uninstall => "卸载",
        _ => return Err(invalid()),
    };
    let (book_witness, book_raw) = disk::witness(&book_path(ctx))?;
    let (lock_witness, lock_raw) = disk::witness(&disk::lock_path(ctx))?;
    let rows = policy::registry(lock_raw.as_deref())?;
    let recipe = select_registered(ctx, key, policy::book(book_raw.as_deref())?, &rows)?;
    let current = package(ctx, &recipe, rows.get(&recipe.install_name), None)?;
    let (_, _, owner_witness) = owners(ctx, &current.id, action)?;
    let old_commit = recipe.source.commit.clone();
    let recipe = if action == Action::Upgrade {
        resolve_upgrade(ctx, recipe, None)?
    } else {
        recipe
    };
    let effect = if action == Action::Uninstall {
        "移除共享载荷后，依赖这处目录的 Agent 将无法再发现该技能"
    } else {
        "更新后的技能仍可由支持共享目录的 Agent 发现"
    };
    confirm(
        &format!(
            "{label} {} · Skills CLI\n来源 https://github.com/{}\n上游成员 {} · {}\n已登记提交 {}\n本次配方 {}\nCodex 用户级共享技能目录\n{effect}",
            recipe.install_name,
            recipe.source.repository,
            recipe.source.member_name,
            recipe.source.skill_directory,
            old_commit,
            recipe.source.commit,
        ),
        false,
    )?;
    if let Some(previous) = crate::operations::recover_before_write(ctx, None)?
        && previous.matches(action, &current.id, None)
        && matches_recommendation(&previous.original, &recipe)?
    {
        return Ok(previous.result);
    }
    let fence = InputFence {
        files: vec![book_witness, lock_witness, owner_witness],
    };
    fence
        .verify()
        .map_err(|_| Error::attention("plan_stale", "已选择的技能配方或安装记录发生变化"))?;
    execute(ctx, recipe, action, false, true, fence)
}

fn source_aliases(ctx: &Context) -> Result<BTreeMap<String, String>> {
    crate::operations::governance::read(ctx, |authority| {
        let Some(raw) = authority.read(
            crate::policy::governance::Dataset::SourceChannels,
            1024 * 1024,
        )?
        else {
            return Ok(BTreeMap::new());
        };
        let value = crate::policy::json::parse(&raw)?;
        if value["schema_version"] != 1 {
            return Err(invalid());
        }
        let Some(ids) = value.get("source_ids") else {
            return Ok(BTreeMap::new());
        };
        ids.as_object()
            .ok_or_else(invalid)?
            .iter()
            .map(|(repo, alias)| {
                Ok((
                    crate::policy::source_git::Repository::parse(repo)?
                        .key()
                        .to_owned(),
                    alias.as_str().ok_or_else(invalid)?.to_owned(),
                ))
            })
            .collect()
    })
}
fn repository_selector(key: &str) -> Option<String> {
    let source = key.strip_prefix("skill:")?;
    let repository = crate::policy::source_git::Repository::parse(source).ok()?;
    Some(format!("skill:{}", repository.key()))
}

fn normalize_selection(
    ctx: &Context,
    book: &Book,
    rows: &BTreeMap<String, Value>,
    request: &mut crate::source::rules::source_select::Request,
) -> Result<()> {
    if let crate::capability::rules::delivery::Selection::Explicit(keys) = &mut request.selection {
        for key in keys {
            if let Some(normalized) = repository_selector(key) {
                *key = normalized;
            }
            if key.starts_with("capability:") || key.starts_with("asset:") {
                let mut matched = vec![];
                for recipe in &book.recipes {
                    if compatibility_keys(ctx, recipe, rows.get(&recipe.install_name))?
                        .contains(key)
                    {
                        matched.push(policy::installation_id(&crate::platform::fs::read_path(
                            &disk::payload(ctx, &recipe.install_name)?,
                        )?)?);
                    }
                }
                match matched.as_slice() {
                    [id] => key.clone_from(id),
                    [] => {}
                    _ => return Err(invalid()),
                }
            }
        }
    }
    Ok(())
}

fn selected_recipes(ctx: &Context, selection: &crate::cli::SkillSelection) -> Result<Vec<Recipe>> {
    use crate::{
        capability::rules::delivery::SourceGroup,
        source::rules::source_select::{Member, Request, resolve},
    };
    let mut request = Request::parse(
        selection.name.as_deref(),
        &selection.only,
        selection.all,
        selection.revision.as_deref(),
        selection.change_id.as_deref(),
    )?;
    if request.change_id.is_some() {
        return Err(Error::attention(
            "native_skills_change_id_unsupported",
            "Native 配方只接受固定安装身份或来源分组，不接受旧 overlay change ID",
        ));
    }
    let book = load_book(ctx)?.1;
    let rows = policy::registry(scoped_files::regular(&disk::lock_path(ctx))?.as_deref())?;
    normalize_selection(ctx, &book, &rows, &mut request)?;
    if selection.all {
        let plugins = crate::source::source_plugins::observe(ctx)?;
        plugins.verify()?;
        if !plugins.unsupported.is_empty() {
            return Err(required());
        }
        let rows = policy::registry(scoped_files::regular(&disk::lock_path(ctx))?.as_deref())?;
        if book.recipes.is_empty()
            || rows
                .keys()
                .any(|name| !book.recipes.iter().any(|r| r.install_name == *name))
        {
            return Err(required());
        }
    }
    let aliases = source_aliases(ctx)?;
    let mut members = vec![];
    let mut groups: BTreeMap<String, SourceGroup> = BTreeMap::new();
    for recipe in &book.recipes {
        let id = policy::installation_id(&crate::platform::fs::read_path(&disk::payload(
            ctx,
            &recipe.install_name,
        )?)?)?;
        let repository = crate::policy::source_git::Repository::parse(&recipe.source.repository)?;
        let key = repository.key().to_owned();
        members.push(Member {
            id: id.clone(),
            canonical_name: recipe.install_name.clone(),
            repository,
            aliases: std::collections::BTreeSet::from([
                recipe.id.clone(),
                format!("skill:{}", recipe.install_name),
                format!("skill:{key}"),
            ]),
        });
        let group = groups.entry(key.clone()).or_insert_with(|| SourceGroup {
            id: format!("source:{key}"),
            aliases: std::collections::BTreeSet::from([key.clone()]),
            members: std::collections::BTreeSet::new(),
        });
        group.members.insert(id);
        if let Some(alias) = aliases.get(&key) {
            group.aliases.insert(alias.clone());
        }
    }
    let groups: Vec<_> = groups.into_values().collect();
    let selected = resolve(&request, &members, &groups).map_err(|error| {
        if error.code == "source_selection_ambiguous_or_missing" {
            missing_source_recipe(&request, &members, &groups, &rows).unwrap_or(error)
        } else {
            error
        }
    })?;
    let by_id = book
        .recipes
        .into_iter()
        .map(|recipe| {
            Ok((
                policy::installation_id(&crate::platform::fs::read_path(&disk::payload(
                    ctx,
                    &recipe.install_name,
                )?)?)?,
                recipe,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    selected
        .into_iter()
        .map(|id| by_id.get(&id).cloned().ok_or_else(invalid))
        .collect()
}

fn missing_source_recipe(
    request: &crate::source::rules::source_select::Request,
    members: &[crate::source::rules::source_select::Member],
    groups: &[crate::capability::rules::delivery::SourceGroup],
    rows: &BTreeMap<String, Value>,
) -> Option<Error> {
    let crate::capability::rules::delivery::Selection::Explicit(keys) = &request.selection else {
        return None;
    };
    let [key] = keys.as_slice() else {
        return None;
    };
    let source = key
        .strip_prefix("source:")
        .or_else(|| key.strip_prefix("skill:"))
        .unwrap_or(key);
    let repo = crate::policy::public_source::repository_subject(source)?;
    if members.iter().any(|member| {
        member.repository.key().eq_ignore_ascii_case(repo)
            || &member.id == key
            || &member.canonical_name == key
            || member.aliases.contains(key)
    }) || groups
        .iter()
        .any(|group| &group.id == key || group.aliases.contains(key))
        || !rows.values().any(|row| {
            row["sourceType"] == "github"
                && row["source"].as_str().is_some_and(|source| {
                    crate::policy::public_source::repository_subject(source)
                        .is_some_and(|name| name.eq_ignore_ascii_case(repo))
                })
        })
    {
        return None;
    }
    Some(
        Error::attention(
            "native_skills_recipe_required",
            format!("已发现 {repo} 的 Skills CLI 安装记录，本机维护条件尚未核对。"),
        )
        .with_guidance(
            "无法用 mo 维护这个来源。",
            format!("运行 mo skill adopt {repo} --dry-run 检查这些安装，或用原安装工具更新。"),
        ),
    )
}

pub(crate) fn matches_recommendation(journal: &Journal, recipe: &Recipe) -> Result<bool> {
    if !is_native(journal) {
        return Ok(false);
    }
    Ok(fingerprint(&frozen(journal)?.recipe)? == fingerprint(recipe)?)
}

pub(crate) fn matches_recipe_file(journal: &Journal, path: &Path) -> Result<bool> {
    if !is_native(journal) {
        return Ok(false);
    }
    let (recipe, _) = read_recipe(path)?;
    matches_recommendation(journal, &recipe)
}

pub(crate) fn matches_upgrade(
    ctx: &Context,
    journal: &Journal,
    selection: &crate::cli::SkillSelection,
) -> Result<bool> {
    if !is_native(journal) {
        return Ok(false);
    }
    if selection
        .name
        .as_deref()
        .is_some_and(|name| name.contains('@'))
    {
        return Ok(false);
    }
    let recipes = selected_recipes(ctx, selection)?;
    let [recipe] = recipes.as_slice() else {
        return Ok(false);
    };
    let previous = frozen(journal)?.recipe;
    Ok(recipe.id == previous.id
        && recipe.source.repository == previous.source.repository
        && selection
            .revision
            .as_ref()
            .is_none_or(|commit| &previous.source.commit == commit))
}

pub(crate) fn validate_recipe_argument(command: &Command) -> Result<()> {
    let Command::Skill {
        command: Some(command),
        ..
    } = command
    else {
        return Ok(());
    };
    let (path, invocation) = match command {
        SkillCmd::Install {
            recipe: Some(recipe),
            ..
        } => (recipe, "mo skill install"),
        SkillCmd::Adopt {
            recipe: Some(recipe),
            ..
        } => (recipe, "mo skill adopt NAME"),
        _ => return Ok(()),
    };
    if path.is_absolute() {
        return Ok(());
    }
    let value = path.to_str().unwrap_or_default();
    let qualified = value.starts_with("https://github.com/") || value.starts_with("github.com/");
    let repository = if qualified
        || !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        crate::policy::public_source::repository_subject(
            value.strip_prefix("github.com/").unwrap_or(value),
        )
    } else {
        None
    };
    let problem = format!("无法读取 --recipe 指定的配方「{}」。", path.display());
    let local_usage = format!("{invocation} --recipe \"/绝对路径/recipe.json\"");
    let (reason, usage) = if let Some(repo) = repository {
        (
            "--recipe 需要本机技能配方 JSON 文件的绝对路径，收到的是 GitHub 仓库。",
            format!("运行 mo skill install {repo}，或用 mo recommend {repo} 查看安装方式。"),
        )
    } else if qualified || value.contains("://") {
        (
            "--recipe 需要本机技能配方 JSON 文件的绝对路径，收到的是网络链接。",
            format!("先取得本地配方文件，再运行 {local_usage}，将示例路径换为实际配方文件。"),
        )
    } else {
        (
            "--recipe 需要以 / 开头的本机技能配方 JSON 文件路径。",
            format!("运行 {local_usage}，将示例路径换为实际配方文件。"),
        )
    };
    Err(Error::new("absolute_path_required", reason).with_guidance(problem, usage))
}

fn explicit_recipe(
    ctx: &Context,
    command: &SkillCmd,
    dry: bool,
    yes: bool,
) -> Result<Option<Value>> {
    if !matches!(command, SkillCmd::Install { .. } | SkillCmd::Adopt { .. }) {
        return Ok(None);
    }
    native_config_scope(ctx)?;
    let value = match command {
        SkillCmd::Install {
            source: None,
            recipe: Some(recipe),
            ..
        } => {
            let (recipe, fence) = read_recipe(recipe)?;
            execute(ctx, recipe, Action::Install, dry, yes, fence)?
        }
        SkillCmd::Adopt {
            id,
            recipe: Some(recipe),
        } => {
            let (recipe, fence) = read_recipe(recipe)?;
            let expected = policy::installation_id(&crate::platform::fs::read_path(
                &disk::payload(ctx, &recipe.install_name)?,
            )?)?;
            if id != &expected
                && id != &recipe.install_name
                && id != &format!("skill:{}", recipe.install_name)
                && !repository_selector(id).is_some_and(|key| {
                    key == format!("skill:{}", recipe.source.repository.to_ascii_lowercase())
                })
            {
                return Err(invalid());
            }
            execute(ctx, recipe, Action::Adopt, dry, yes, fence)?
        }
        SkillCmd::Install {
            source: Some(source),
            ..
        } => journey::install(ctx, source, dry, yes)?,
        SkillCmd::Adopt { id, recipe: None } => journey::adopt(ctx, id, dry, yes)?,
        _ => return Ok(None),
    };
    Ok(Some(value))
}

pub(crate) fn has_supported_runner(ctx: &Context) -> bool {
    crate::skills::public_skill::runner(ctx).is_ok()
}

pub(crate) fn install_repository(ctx: &Context, source: &str) -> Result<Value> {
    journey::install(ctx, source, false, false)
}

pub(crate) use journey::{RepositoryAction, repository_action, upgrade_repository};

pub(super) enum SharedPlan {
    Install(journey::InstallPlan),
    Maintain(SharedMaintenance),
}

pub(super) struct SharedMaintenance {
    source: String,
    fence: InputFence,
    members: Vec<(Recipe, Recipe)>,
    action: Action,
}

pub(super) fn plan_shared(
    ctx: &Context,
    source: &str,
    operation: crate::domain::resource::CapabilityAction,
    revision: Option<&str>,
) -> Result<SharedPlan> {
    use crate::domain::resource::CapabilityAction as Operation;
    native_config_scope(ctx)?;
    let source = source.strip_prefix("source:").unwrap_or(source);
    if operation == Operation::Install {
        let plan = journey::plan_install(ctx, source)?;
        plan.verify_batch()?;
        return Ok(SharedPlan::Install(plan));
    }
    let (source, requested) = if source.contains('@') {
        if revision.is_some() || operation != Operation::Upgrade {
            return Err(invalid());
        }
        let reference = crate::skills::public_skill::Request::parse(source)?;
        let commit = reference.resolve()?;
        (
            reference.repository.key().to_owned(),
            Some(commit.as_str().to_owned()),
        )
    } else {
        (source.to_owned(), revision.map(str::to_owned))
    };
    let installed = journey::installed_selection(ctx, &source)?.ok_or_else(required)?;
    let action = match operation {
        Operation::Upgrade => Action::Upgrade,
        Operation::Uninstall => Action::Uninstall,
        _ => return Err(required()),
    };
    let mut members = vec![];
    for old in installed.managed.into_iter().chain(installed.unowned) {
        let snapshot = checked_snapshot(ctx, &old)?;
        validate_action(ctx, &old, action, &snapshot)?;
        let mut next = if action == Action::Upgrade {
            resolve_upgrade(ctx, old.clone(), requested.as_deref())?
        } else {
            old.clone()
        };
        if policy::generated(&next) && requested.is_some() {
            next.source.tracking_ref = None;
        }
        members.push((old, next));
    }
    installed.fence.verify()?;
    Ok(SharedPlan::Maintain(SharedMaintenance {
        source,
        fence: installed.fence,
        members,
        action,
    }))
}

impl SharedPlan {
    pub(super) fn run(self, ctx: &Context) -> Result<Value> {
        match self {
            Self::Install(plan) => plan.run(ctx, false, true, true),
            Self::Maintain(plan) => plan.run(ctx),
        }
    }
}

impl SharedMaintenance {
    fn run(self, ctx: &Context) -> Result<Value> {
        self.fence.verify()?;
        if let Some(value) = journey::try_adopt(ctx, &self.source, false, true, false)?
            && value["incomplete"] == true
        {
            return Ok(value);
        }
        self.fence.verify()?;
        let current = journey::installed_selection(ctx, &self.source)?.ok_or_else(invalid)?;
        if !current.unowned.is_empty() || current.managed.len() != self.members.len() {
            return Err(invalid());
        }
        for (old, _) in &self.members {
            let actual = current
                .managed
                .iter()
                .find(|row| row.install_name == old.install_name)
                .ok_or_else(invalid)?;
            let mut expected = old.clone();
            if expected.runner.sha256.is_none() {
                expected.runner.sha256.clone_from(&actual.runner.sha256);
            }
            disk::same(actual, &expected)?;
        }
        let mut writes = false;
        for (old, mut next) in self.members {
            let actual = current
                .managed
                .iter()
                .find(|row| row.install_name == old.install_name)
                .ok_or_else(invalid)?;
            next.runner.sha256.clone_from(&actual.runner.sha256);
            if self.action == Action::Upgrade && next == *actual {
                verify_managed_skill(ctx, actual)?;
                continue;
            }
            let report = execute(ctx, next, self.action, false, true, InputFence::default())?;
            if report["journal"]["phase"] != "succeeded" {
                return Ok(report);
            }
            writes = true;
        }
        Ok(json!({"status":if writes {"changed"} else {"current"}}))
    }
}

pub(crate) fn adopt_repository(ctx: &Context, source: &str) -> Result<Value> {
    let request = crate::skills::public_skill::Request::parse(source)?;
    journey::adopt(ctx, request.repository.key(), false, false)
}

fn native_config_scope(ctx: &Context) -> Result<()> {
    if ctx.chezmoi.source.is_some()
        || ctx.chezmoi.destination.is_some()
        || ctx.chezmoi.config.is_some()
    {
        return Err(Error::new(
            "config_scope_unsupported",
            "Native Skills 不接受 chezmoi 配置范围参数",
        ));
    }
    Ok(())
}

fn upgrade(
    ctx: &Context,
    selection: &crate::cli::SkillSelection,
    dry: bool,
    yes: bool,
) -> Result<Value> {
    let mut selection = selection.clone();
    let explicit_source = selection
        .name
        .as_deref()
        .and_then(|name| name.rsplit_once('@'));
    if explicit_source.is_some() {
        if selection.revision.is_some() || selection.all || !selection.only.is_empty() {
            return Err(Error::attention(
                "source_revision_selection",
                "指定 @版本时，请只选择这一个仓库。",
            ));
        }
        let requested = crate::skills::public_skill::Request::parse(
            selection.name.as_deref().unwrap_or_default(),
        )?;
        let repository = requested.repository.key().to_owned();
        if let Some(mut report) = journey::try_adopt(ctx, &repository, dry, yes, true)? {
            report["next"] = json!(format!(
                "mo skill upgrade {}",
                selection.name.as_deref().unwrap_or_default()
            ));
            return Ok(report);
        }
        selection.revision = Some(requested.resolve()?.as_str().to_owned());
        selection.name = Some(repository);
    } else if let Some(target) = selection.name.as_deref()
        && selection.only.is_empty()
        && !selection.all
        && selection.change_id.is_none()
        && selection.revision.is_none()
        && let Some(report) = journey::try_adopt(ctx, target, dry, yes, true)?
    {
        return Ok(report);
    }
    let recipes = selected_recipes(ctx, &selection)?;
    run_upgrades(ctx, recipes, selection.revision.as_deref(), dry, yes)
}

fn run_upgrades(
    ctx: &Context,
    recipes: Vec<Recipe>,
    revision: Option<&str>,
    dry: bool,
    yes: bool,
) -> Result<Value> {
    let mut recipes = recipes.into_iter();
    let single = recipes.len() == 1;
    let mut results = vec![];
    let mut incomplete = false;
    for recipe in recipes.by_ref() {
        let id = recipe.id.clone();
        let name = recipe.install_name.clone();
        match upgrade_recipe(ctx, recipe, revision, dry, yes) {
            Ok(result) => {
                incomplete = !dry
                    && result["journal"]["phase"] != "succeeded"
                    && !matches!(
                        result["status"].as_str(),
                        Some("up-to-date" | "fixed-version")
                    );
                results.push(result);
            }
            Err(error) if single => return Err(error),
            Err(error) => {
                results.push(json!({"recipe":id,"name":name,"status":"unknown","error":error}));
                incomplete = true;
            }
        }
        if incomplete {
            break;
        }
    }
    let partial = incomplete
        && results
            .iter()
            .any(|result| result["journal"]["phase"] == "succeeded");
    let (unattempted, unattempted_names): (Vec<_>, Vec<_>) = recipes
        .map(|recipe| (recipe.id, recipe.install_name))
        .unzip();
    Ok(
        json!({"backend":"rust","results":results,"incomplete":incomplete,"partial":partial,
        "unattempted":unattempted,"unattemptedNames":unattempted_names}),
    )
}

fn upgrade_recipe(
    ctx: &Context,
    recipe: Recipe,
    revision: Option<&str>,
    dry: bool,
    yes: bool,
) -> Result<Value> {
    let old_commit = recipe.source.commit.clone();
    let generated = policy::generated(&recipe);
    if generated && revision.is_none() && recipe.source.tracking_ref.is_none() {
        verify_managed_skill(ctx, &recipe)?;
        return Ok(
            json!({"name":recipe.install_name,"repository":recipe.source.repository,"status":"fixed-version"}),
        );
    }
    let was_tracking = recipe.source.tracking_ref.is_some();
    let mut recipe = resolve_upgrade(ctx, recipe, revision)?;
    if generated && revision.is_some() {
        recipe.source.tracking_ref = None;
    }
    if generated
        && old_commit == recipe.source.commit
        && was_tracking == recipe.source.tracking_ref.is_some()
    {
        verify_managed_skill(ctx, &recipe)?;
        return Ok(json!({"name":recipe.install_name,"status":"up-to-date"}));
    }
    if generated && !dry {
        crate::operations::confirm(
            &format!(
                "更新 {}\n来源：{}\n位置：{}",
                recipe.install_name,
                recipe.source.repository,
                disk::payload(ctx, &recipe.install_name)?.display()
            ),
            yes,
        )?;
    }
    execute(
        ctx,
        recipe,
        Action::Upgrade,
        dry,
        yes,
        InputFence::default(),
    )
}

fn verify_managed_skill(ctx: &Context, recipe: &Recipe) -> Result<()> {
    let rows = policy::registry(scoped_files::regular(&disk::lock_path(ctx))?.as_deref())?;
    let Some(row) = rows.get(&recipe.install_name) else {
        return Err(Error::attention(
            "skill_not_installed",
            format!("本机找不到 {} 的安装记录。", recipe.install_name),
        )
        .with_guidance(
            "没有可以更新的安装。",
            format!(
                "运行 mo skill install {} 重新选择并安装。",
                recipe.source.repository
            ),
        ));
    };
    let current = package(ctx, recipe, Some(row), None)?;
    owners(ctx, &current.id, Action::Upgrade)?;
    Ok(())
}

fn resolve_upgrade(ctx: &Context, mut recipe: Recipe, revision: Option<&str>) -> Result<Recipe> {
    if let Some(commit) = revision {
        recipe.source.commit = crate::skills::rules::skills::revision(commit)?;
    } else if let Some(reference) = &recipe.source.tracking_ref
        && policy::generated(&recipe)
    {
        let source = format!("{}@{reference}", recipe.source.repository);
        crate::skills::public_skill::Request::parse(&source)?
            .resolve()?
            .as_str()
            .clone_into(&mut recipe.source.commit);
    } else if recipe.source.tracking_ref.is_some() {
        let cache = check_cache(ctx)?;
        let id = policy::installation_id(&crate::platform::fs::read_path(&disk::payload(
            ctx,
            &recipe.install_name,
        )?)?)?;
        let selected_commit = &cache
            .index
            .get(
                &cache_scope(ctx),
                &id,
                &policy::locator(&recipe, &id)?,
                crate::operations::store::now(),
            )?
            .commit;
        recipe.source.commit.clone_from(selected_commit);
    }
    Ok(recipe)
}

pub fn run(ctx: &Context, command: &Command, dry: bool, yes: bool) -> Result<Option<Value>> {
    let Command::Skill {
        project,
        command: Some(command),
        ..
    } = command
    else {
        return Ok(None);
    };
    if let Some(value) = explicit_recipe(ctx, command, dry, yes)? {
        return Ok(Some(value));
    }
    let value = match command {
        SkillCmd::Detail(detail) if project.is_none() => {
            let term = &detail.target;
            let key = if term.contains(':') {
                term.clone()
            } else {
                format!("skill:{term}")
            };
            return native_info(ctx, &key);
        }
        SkillCmd::Remove { id, .. } => execute(
            ctx,
            select(ctx, id)?,
            Action::Uninstall,
            dry,
            yes,
            InputFence::default(),
        )?,
        SkillCmd::Upgrade(selection) => upgrade(ctx, selection, dry, yes)?,
        SkillCmd::Refresh(selection) => refresh(ctx, selection, dry)?,
        SkillCmd::Outdated { name, only } => outdated(ctx, name.as_deref(), only)?,
        _ => return Ok(None),
    };
    Ok(Some(value))
}

pub fn app_scoped(
    ctx: &Context,
    key: &str,
    exact: bool,
    version: Option<&str>,
    action: Action,
    dry: bool,
    yes: bool,
) -> Result<Option<Value>> {
    let Some(package) =
        crate::capability::capability_local::resolve_package(ctx, key, false, exact)?
    else {
        return Ok(None);
    };
    if !matches!(
        package.provider,
        Provider::CapabilityObserved {
            capability: CapabilityKind::SkillsCli,
            ..
        }
    ) {
        return Ok(None);
    }
    native_config_scope(ctx)?;
    if action == Action::Upgrade {
        let identity = capability_local::canonical_identity(&package)?;
        let selection = crate::cli::SkillSelection {
            name: Some(identity),
            revision: version.map(str::to_owned),
            only: vec![],
            all: false,
            change_id: None,
            agents: crate::cli::SkillAgents::default(),
            switch_source: false,
        };
        return Ok(Some(upgrade(ctx, &selection, dry, yes)?));
    }
    execute(
        ctx,
        select(ctx, key)?,
        action,
        dry,
        yes,
        InputFence::default(),
    )
    .map(Some)
}

pub fn catalog_summary(ctx: &Context) -> Result<Value> {
    let (_, book) = load_book(ctx)?;
    let rows = policy::registry(scoped_files::regular(&disk::lock_path(ctx))?.as_deref())?;
    let mut sources = vec![];
    let pending = pending_delivery(ctx)?;
    for (name, row) in rows {
        let recipe = book.recipes.iter().find(|r| r.install_name == name);
        let id = policy::installation_id(&crate::platform::fs::read_path(&disk::payload(
            ctx, &name,
        )?)?)?;
        let active = pending
            .as_ref()
            .filter(|(key, _)| key == &id)
            .map(|(_, value)| value);
        sources.push(json!({"id":id,"name":name,"owner":"skills-cli","nativeSource":crate::policy::catalog::resources::source_label(row["source"].as_str().unwrap_or_default()),"nativeSourceType":row["sourceType"],"nativeRawSkillPath":row.get("skillPath"),"nativeCommit":row.get("commit"),"recipe":active.and_then(|v|v.get("recipe")).cloned().or(recipe.map(recipe_summary).transpose()?),"delivery":active,"maintenance":if active.is_some(){"pending-native-operation"}else if recipe.is_some(){"fixed-recipe"}else{"recipe-required"}}));
    }
    Ok(
        json!({"maintainable_scope":"native-skills-fixed-recipes","sources":sources,"persistentWrites":false}),
    )
}
pub(crate) fn native_info(ctx: &Context, key: &str) -> Result<Option<Value>> {
    let Some(package) =
        crate::capability::capability_local::resolve_package(ctx, key, false, false)?
    else {
        return Ok(None);
    };
    if !matches!(
        package.provider,
        Provider::CapabilityObserved {
            capability: CapabilityKind::SkillsCli,
            ..
        }
    ) {
        return Ok(None);
    }
    let (_, observation) = observation(&package)?;
    let (_, book) = load_book(ctx)?;
    let recipe = book.recipes.iter().find(|r| r.install_name == package.name);
    let delivery = delivery_summary(ctx, &package, recipe, &observation)?;
    let summary = delivery
        .get("recipe")
        .cloned()
        .or(recipe.map(recipe_summary).transpose()?);
    Ok(Some(
        json!({"package":package,"observation":observation,"recipe":summary,
        "delivery":delivery,"owner":"skills-cli","loaded":"unknown","persistentWrites":false}),
    ))
}
fn delivery_summary(
    ctx: &Context,
    package: &Package,
    recipe: Option<&Recipe>,
    observation: &Observation,
) -> Result<Value> {
    if let Some((id, value)) = pending_delivery(ctx)?
        && id == package.id
    {
        return Ok(value);
    }
    let Some(raw) = scoped_files::regular(&receipt_path(ctx, &package.id))? else {
        return Ok(json!({"stage":"unrecorded"}));
    };
    let step: StepReceipt = policy::decode(&raw)?;
    if step.schema_version != 1
        || step.installation_id != package.id
        || !matches!(step.stage.as_str(), "verified" | "adopted-baseline")
    {
        return Err(invalid());
    }
    let common = scoped_files::regular(&ctx.state.join("receipts.json"))?
        .as_deref()
        .map(policy::decode::<Receipts>)
        .transpose()?;
    let current = common
        .as_ref()
        .and_then(|r| r.installations.get(&package.id));
    let recipe_matches =
        recipe.map(policy::recipe_hash).transpose()?.as_ref() == Some(&step.recipe_hash);
    let matches = current.is_some_and(|r| r.operation_id == step.operation_id)
        && step.observation_hash.as_ref() == Some(&fingerprint(observation)?)
        && recipe_matches;
    Ok(if matches {
        json!({"stage":step.stage,"operationId":step.operation_id,"recipeHash":step.recipe_hash})
    } else {
        json!({"stage":"unknown","reason":"receipt_no_longer_matches_current_installation"})
    })
}
fn pending_delivery(ctx: &Context) -> Result<Option<(String, Value)>> {
    let Some(raw) = scoped_files::regular(&ctx.state.join("active.json"))? else {
        return Ok(None);
    };
    let journal: Journal = policy::decode(&raw)?;
    crate::operations::store::validate_journal(&journal)?;
    if !is_native(&journal) || matches!(journal.phase, Phase::Succeeded | Phase::Failed) {
        return Ok(None);
    }
    if journal.home != ctx.home
        || journal.state != ctx.state
        || policy::native_scope(&journal.scope)? != policy::native_scope(&ctx.scope)?
    {
        return Err(invalid());
    }
    let frozen = frozen(&journal)?;
    original_state(&frozen)?;
    let progress = progress(&journal)?;
    let stage = match progress.stage.as_str() {
        "native-succeeded" | "customization-pending" => progress.stage.as_str(),
        _ if journal
            .process_group
            .is_some_and(crate::platform::process::group_alive) =>
        {
            "native-running"
        }
        _ => "native-unknown",
    };
    Ok(Some((
        journal.plan.package.id.clone(),
        json!({"stage":stage,"operationId":journal.id,
        "recipeHash":policy::recipe_hash(&frozen.recipe)?,"recipe":recipe_summary(&frozen.recipe)?}),
    )))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckCache {
    schema_version: u32,
    #[serde(deserialize_with = "crate::policy::json::object")]
    index: crate::source::rules::source_index::Index,
    baselines: BTreeMap<String, String>,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    native_checks: BTreeMap<String, NativeCheck>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCheck {
    code: Option<i32>,
    uncertain: bool,
    scope: String,
}
fn cache_scope(ctx: &Context) -> crate::source::rules::source_index::Scope {
    crate::source::rules::source_index::Scope {
        home: ctx.home.clone(),
        native_lock: disk::lock_path(ctx),
        definitions: book_path(ctx),
    }
}
fn check_cache(ctx: &Context) -> Result<CheckCache> {
    let scope = cache_scope(ctx);
    let Some(raw) = scoped_files::regular(&ctx.cache.join("native-skills-checks.json"))? else {
        return Ok(CheckCache {
            schema_version: 1,
            index: crate::source::rules::source_index::Index::empty(scope)?,
            baselines: BTreeMap::new(),
            native_checks: BTreeMap::new(),
        });
    };
    if crate::source::rules::source_index::legacy(&raw)? {
        return Err(Error::attention(
            "native_skills_legacy_index",
            "旧索引不能作为固定配方候选，请重新 update",
        ));
    }
    let cache: CheckCache = policy::decode(&raw)?;
    crate::source::rules::source_index::read(
        &serde_json::to_vec(&cache.index)?,
        &scope,
        crate::operations::store::now(),
    )?;
    if cache.schema_version != 1
        || cache.native_checks.len() != cache.index.entries.len()
        || cache.native_checks.iter().any(|(id, c)| {
            !cache.index.entries.contains_key(id)
                || !matches!(
                    c.scope.as_str(),
                    "registered-sha-pin" | "registered-ref" | "local-registration-only"
                )
        })
        || cache.baselines.len() != cache.index.entries.len()
        || cache.baselines.iter().any(|(id, hash)| {
            !cache.index.entries.contains_key(id)
                || !crate::policy::config_local::valid_digest(hash)
        })
    {
        return Err(invalid());
    }
    Ok(cache)
}
fn cached_row(
    ctx: &Context,
    recipe: &Recipe,
    cache: &CheckCache,
    rows: &BTreeMap<String, Value>,
) -> Result<Value> {
    let id = policy::installation_id(&crate::platform::fs::read_path(&disk::payload(
        ctx,
        &recipe.install_name,
    )?)?)?;
    let locator = policy::locator(recipe, &id)?;
    let Some(row) = rows.get(&recipe.install_name) else {
        return Ok(
            json!({"id":id,"name":recipe.install_name,"status":"missing","reason":"native_registration_missing"}),
        );
    };
    let Ok(candidate) = cache.index.get(
        &cache_scope(ctx),
        &id,
        &locator,
        crate::operations::store::now(),
    ) else {
        return Ok(
            json!({"id":id,"name":recipe.install_name,"status":"unknown","reason":"index_missing_or_stale"}),
        );
    };
    let native_folder_changed = if recipe.derived.is_none() {
        let repo = crate::policy::source_git::Repository::parse(
            row["source"].as_str().ok_or_else(invalid)?,
        )?;
        if row["sourceType"] != "github"
            || repo.key() != recipe.source.repository.to_ascii_lowercase()
            || policy::skill_directory(row["skillPath"].as_str().ok_or_else(invalid)?)?
                != recipe.source.skill_directory
        {
            return Ok(
                json!({"id":id,"name":recipe.install_name,"status":"unknown","reason":"native_source_changed"}),
            );
        }
        let hash = row["skillFolderHash"].as_str().ok_or_else(invalid)?;
        if hash.len() == 40 && recipe.source.skill_directory == "." {
            let installed = row
                .get("commit")
                .and_then(Value::as_str)
                .or_else(|| row.get("ref").and_then(Value::as_str));
            if installed != Some(recipe.source.commit.as_str())
                || !hash.eq_ignore_ascii_case(&recipe.source.commit)
            {
                return Ok(
                    json!({"id":id,"name":recipe.install_name,"status":"unknown","reason":"native_baseline_unproven"}),
                );
            }
            false
        } else if hash.len() == 40 {
            !hash.eq_ignore_ascii_case(&candidate.skill_tree)
        } else {
            if row["commit"] != recipe.source.commit && row["ref"] != recipe.source.commit {
                return Ok(
                    json!({"id":id,"name":recipe.install_name,"status":"unknown","reason":"native_baseline_unproven"}),
                );
            }
            false
        }
    } else {
        false
    };
    let outdated =
        native_folder_changed || cache.baselines.get(&id) != Some(&candidate.payload_sha256);
    let uncertain = cache
        .native_checks
        .get(&id)
        .is_none_or(|c| c.uncertain || c.code != Some(0));
    Ok(
        json!({"id":id,"name":recipe.install_name,"commit":candidate.commit,"contentIdentity":candidate.payload_sha256,
        "status":if uncertain {"unknown"}else if outdated {"outdated"}else{"current"},"outdated":outdated,
        "repositoryAhead":recipe.source.commit!=candidate.commit,"nativePinCheck":cache.native_checks.get(&id),"checkedAt":candidate.checked_at}),
    )
}
fn native_check(
    ctx: &Context,
    store: &Store,
    recipe: &Recipe,
    snapshot: &Snapshot,
) -> Result<NativeCheck> {
    if recipe.runner.sha256.as_deref() != Some(disk::runner_stamp(recipe)?.as_str()) {
        return Err(invalid());
    }
    let env = disk::frozen_environment(ctx)?;
    let environment = EnvironmentGuards::capture(ctx)?;
    let directory = crate::platform::fs::Directory::open(&ctx.state)?;
    let guard = || {
        store.verify_namespace()?;
        environment.verify()?;
        directory.verify()?;
        snapshot.before(ctx, recipe)
    };
    let mut version = std::process::Command::new(&recipe.runner.node);
    version
        .arg("--version")
        .env_clear()
        .envs(&env)
        .current_dir(&ctx.home)
        .stdin(std::process::Stdio::null())
        .process_group(0);
    let output = query_capture(&mut version, &directory, guard)?;
    if output.code != Some(0) || output.uncertain {
        return Err(invalid());
    }
    policy::node_version(std::str::from_utf8(&output.stdout).map_err(|_| invalid())?)?;
    let mut query = std::process::Command::new(&recipe.runner.node);
    query
        .args([
            recipe.runner.entry.as_os_str(),
            std::ffi::OsStr::new("check"),
            std::ffi::OsStr::new("--global"),
            std::ffi::OsStr::new(&recipe.install_name),
        ])
        .env_clear()
        .envs(&env)
        .current_dir(&ctx.home)
        .stdin(std::process::Stdio::null())
        .process_group(0);
    let output = query_capture(&mut query, &directory, guard)?;
    snapshot.before(ctx, recipe)?;
    if recipe.runner.sha256.as_deref() != Some(disk::runner_stamp(recipe)?.as_str()) {
        return Err(invalid());
    }
    Ok(NativeCheck {
        code: output.code,
        uncertain: output.uncertain,
        scope: if recipe.derived.is_some() {
            "local-registration-only"
        } else if snapshot
            .rows
            .get(&recipe.install_name)
            .and_then(|r| r.get("ref"))
            .and_then(Value::as_str)
            .is_some_and(|r| crate::policy::source_git::ObjectId::parse(r).is_ok())
        {
            "registered-sha-pin"
        } else {
            "registered-ref"
        }
        .into(),
    })
}
fn refresh(ctx: &Context, selection: &crate::cli::SkillSelection, dry: bool) -> Result<Value> {
    refresh_with_candidate(ctx, selection, dry, native_skills_assets::candidate)
}

fn refresh_with_candidate(
    ctx: &Context,
    selection: &crate::cli::SkillSelection,
    dry: bool,
    mut read_candidate: impl FnMut(&Context, &Recipe) -> Result<(String, String, String)>,
) -> Result<Value> {
    let recipes = selected_recipes(ctx, selection)?;
    if dry {
        return Ok(
            json!({"mode":"dry-run","persistentWrites":false,"networkInvoked":false,"recipes":recipes}),
        );
    }
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    let authority = crate::operations::governance::capture(ctx)?;
    authority.ensure_settled()?;
    if !crate::policy::governance::cache_allowed(
        &crate::operations::governance::locations(ctx),
        &ctx.cache,
    ) {
        return Err(invalid());
    }
    let mut cache = check_cache(ctx)?;
    let mut candidates = vec![];
    for recipe in &recipes {
        let snapshot = disk::snapshot(ctx, recipe)?;
        let check = native_check(ctx, &store, recipe, &snapshot)?;
        let baseline = read_candidate(
            ctx,
            &Recipe {
                source: policy::Source {
                    tracking_ref: None,
                    ..recipe.source.clone()
                },
                ..recipe.clone()
            },
        )?;
        let candidate = if recipe.source.tracking_ref.is_some() {
            read_candidate(ctx, recipe)?
        } else {
            baseline.clone()
        };
        snapshot.before(ctx, recipe)?;
        let id = policy::installation_id(&crate::platform::fs::read_path(&disk::payload(
            ctx,
            &recipe.install_name,
        )?)?)?;
        candidates.push(crate::source::rules::source_index::Candidate {
            member: id.clone(),
            repository: recipe.source.repository.to_ascii_lowercase(),
            locator_sha256: policy::locator(recipe, &id)?,
            requested_ref: recipe.source.tracking_ref.clone(),
            commit: candidate.0,
            skill_tree: candidate.2,
            payload_sha256: candidate.1,
            checked_at: crate::operations::store::now(),
        });
        cache.baselines.insert(id.clone(), baseline.1);
        cache.native_checks.insert(id, check);
    }
    cache.index = cache.index.merge(
        &cache_scope(ctx),
        candidates,
        crate::operations::store::now(),
    )?;
    let native_rows = policy::registry(scoped_files::regular(&disk::lock_path(ctx))?.as_deref())?;
    let rows = recipes
        .iter()
        .map(|r| cached_row(ctx, r, &cache, &native_rows))
        .collect::<Result<Vec<_>>>()?;
    let path = ctx.cache.join("native-skills-checks.json");
    let raw = scoped_files::regular(&path)?;
    disk::write_cas(&path, raw.as_deref(), &disk::encode(&cache)?)?;
    store.verify_namespace()?;
    Ok(
        json!({"backend":"rust","incomplete":rows.iter().any(|r|r["status"]=="unknown"),"results":rows,"rows":rows,"installationWrites":false,"persistentWrites":true}),
    )
}
fn outdated(ctx: &Context, name: Option<&str>, only: &[String]) -> Result<Value> {
    let cache = check_cache(ctx)?;
    let (_, book) = load_book(ctx)?;
    if book.recipes.is_empty() {
        return unconfigured_outdated(ctx, name, only);
    }
    let recipes = if name.is_none() && only.is_empty() {
        book.recipes
    } else {
        selected_recipes(
            ctx,
            &crate::cli::SkillSelection {
                name: name.map(str::to_owned),
                only: only.to_vec(),
                all: false,
                revision: None,
                change_id: None,
                agents: crate::cli::SkillAgents::default(),
                switch_source: false,
            },
        )?
    };
    let native_rows = policy::registry(scoped_files::regular(&disk::lock_path(ctx))?.as_deref())?;
    let results = recipes
        .iter()
        .map(|recipe| cached_row(ctx, recipe, &cache, &native_rows))
        .collect::<Result<Vec<_>>>()?;
    let incomplete = results
        .iter()
        .any(|r| matches!(r["status"].as_str(), Some("unknown" | "missing")));
    Ok(
        json!({"backend":"rust","resources":results,"results":results,"incomplete":incomplete,
        "cache":{"status":if incomplete {"unknown"} else {"verified"}},"persistentWrites":false,"networkInvoked":false}),
    )
}
fn unconfigured_outdated(ctx: &Context, name: Option<&str>, only: &[String]) -> Result<Value> {
    let rows = policy::registry(scoped_files::regular(&disk::lock_path(ctx))?.as_deref())?;
    let keys = name
        .into_iter()
        .chain(only.iter().map(String::as_str))
        .collect::<Vec<_>>();
    let mut resources = vec![];
    for (native_id, row) in rows {
        let id = policy::installation_id(&crate::platform::fs::read_path(&disk::payload(
            ctx, &native_id,
        )?)?)?;
        if !keys.is_empty()
            && !keys.iter().any(|key| {
                *key == native_id
                    || *key == id
                    || *key == format!("skill:{native_id}")
                    || *key == format!("source:{}", row["source"].as_str().unwrap_or_default())
            })
        {
            continue;
        }
        resources.push(json!({"id":id,"native_id":native_id,"name":native_id,"status":"unknown","reason":"recipe_required"}));
    }
    Ok(
        json!({"backend":"rust","resources":resources,"results":resources,"incomplete":true,
        "cache":{"status":"unknown","reason":"index_missing"},"persistentWrites":false,"networkInvoked":false}),
    )
}

#[cfg(test)]
#[path = "../../tests/unit/native_skills.rs"]
mod tests;
