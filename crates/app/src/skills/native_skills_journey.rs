use super::{
    InputFence, Recipe, checked_snapshot, disk, execute, load_book, native_config_scope, policy,
    run_upgrades, validate_action, verify_managed_skill,
};
use crate::{
    context::Context,
    domain::Action,
    error::{Error, Result},
    skills::public_skill,
};
use serde_json::{Value, json};

pub(in crate::skills) struct InstallPlan {
    source: String,
    existing: Vec<String>,
    existing_proofs: Vec<disk::Object>,
    available: Vec<Recipe>,
    all_verified: bool,
    fence: InputFence,
}

pub(super) fn install(ctx: &Context, source: &str, dry: bool, yes: bool) -> Result<Value> {
    plan_install(ctx, source)?.run(ctx, dry, yes, false)
}

pub(super) fn plan_install(ctx: &Context, source: &str) -> Result<InstallPlan> {
    native_config_scope(ctx)?;
    let request = public_skill::Request::parse(source)?;
    let mut recipes =
        crate::terminal::progress::during("查找仓库中可安装的技能", || {
            request.discover(ctx)
        })?;
    let (witness, raw) = disk::witness(&disk::lock_path(ctx))?;
    let rows = policy::registry(raw.as_deref())?;
    let (_, book) = load_book(ctx)?;
    let mut existing = vec![];
    let mut existing_proofs = vec![];
    let mut all_verified = true;
    let mut available = Vec::with_capacity(recipes.len());
    for recipe in recipes.drain(..) {
        let before = disk::capture(&disk::payload(ctx, &recipe.install_name)?)?;
        if rows.contains_key(&recipe.install_name) || before.present() {
            let verified = before.present()
                && rows.get(&recipe.install_name).is_some_and(|row| {
                    public_skill::from_record(ctx, &recipe.install_name, row, recipe.runner.clone())
                        .is_ok_and(|installed| {
                            installed.source.repository == recipe.source.repository
                                && installed.source.skill_directory == recipe.source.skill_directory
                                && (recipe.source.tracking_ref.is_some()
                                    || installed.source.commit == recipe.source.commit)
                        })
                });
            all_verified &= verified;
            if verified {
                existing_proofs.push(before);
            }
            existing.push(recipe.install_name);
            continue;
        }
        let retained = book
            .recipes
            .iter()
            .find(|item| item.install_name == recipe.install_name);
        if let Some(registered) = retained {
            if let Some(reinstall) = preserve_installation(recipe, registered) {
                available.push(reinstall);
            } else {
                all_verified = false;
                existing.push(registered.install_name.clone());
            }
            continue;
        }
        available.push(recipe);
    }
    Ok(InstallPlan {
        source: request.repository.key().into(),
        existing,
        existing_proofs,
        available,
        all_verified,
        fence: InputFence {
            files: vec![witness],
        },
    })
}

impl InstallPlan {
    pub(super) fn verify_batch(&self) -> Result<()> {
        if !self.all_verified {
            return Err(Error::attention(
                "skill_install_conflict",
                "已有同名技能，无法确认它来自所选来源和版本。请用 mo skill --details 核对安装位置。",
            ));
        }
        self.fence.verify()?;
        for proof in &self.existing_proofs {
            proof.verify()?;
        }
        Ok(())
    }

    pub(super) fn run(self, ctx: &Context, dry: bool, yes: bool, batch: bool) -> Result<Value> {
        if batch {
            self.verify_batch()?;
        }
        let Self {
            source,
            existing,
            existing_proofs: _,
            available,
            all_verified,
            fence,
        } = self;
        if available.is_empty() && !dry {
            if batch && all_verified {
                return Ok(json!({"status":"current"}));
            }
            return Err(Error::attention(
                "skills_already_present",
                "这批技能在本机已有文件或安装记录。",
            )
            .with_guidance(
                "没有新安装。",
                format!("运行 mo skill upgrade {source} 查看更新。"),
            ));
        }
        let preview = json!({"mode":"dry-run","skillAction":"install","source":source,
        "available":available.iter().map(|item|&item.install_name).collect::<Vec<_>>(),
        "alreadyPresent":existing,"directory":ctx.home.join(".agents/skills"),
        "scope":"user-shared","persistentWrites":false});
        if dry {
            return Ok(preview);
        }
        let selected = if yes || available.len() == 1 {
            (0..available.len()).collect()
        } else {
            eprintln!("选择要安装的技能（{} 项）", available.len());
            let labels = available
                .iter()
                .map(|item| item.install_name.clone())
                .collect::<Vec<_>>();
            crate::terminal::picker::select(&labels, 0, true, "", |_| None)?
        };
        let mut selected: Vec<_> = selected
            .into_iter()
            .filter_map(|index| available.get(index).cloned())
            .collect();
        if selected.is_empty() {
            return Err(Error::attention("cancelled", "已取消安装"));
        }
        crate::terminal::progress::during("核对安装工具", || seal_runners(&mut selected))?;
        let names = selected
            .iter()
            .map(|item| item.install_name.as_str())
            .collect::<Vec<_>>()
            .join("、");
        let skip = if existing.is_empty() {
            String::new()
        } else {
            format!("\n已有技能：{}", existing.join("、"))
        };
        crate::operations::confirm(
            &format!(
                "安装并接管 {}\n技能：{names}\n位置：{}\n这些技能可由支持共享目录的 Agent 读取。{skip}",
                source,
                ctx.home.join(".agents/skills").display()
            ),
            yes,
        )?;
        crate::operations::recover_before_write(ctx, None)?;
        fence.verify()?;
        let mut done = run_members(ctx, selected, Action::Install, &InputFence::default())?;
        done["skillAction"] = json!("install");
        done["source"] = json!(source);
        Ok(done)
    }
}

fn preserve_installation(recipe: Recipe, registered: &Recipe) -> Option<Recipe> {
    (policy::generated(registered)
        && registered.source.repository == recipe.source.repository
        && registered.source.skill_directory == recipe.source.skill_directory
        && registered.source.member_name == recipe.source.member_name)
        .then(|| Recipe {
            source: recipe.source,
            ..registered.clone()
        })
}

fn seal_runners(recipes: &mut [Recipe]) -> Result<()> {
    let mut stamps = std::collections::BTreeMap::new();
    for recipe in recipes
        .iter_mut()
        .filter(|recipe| recipe.runner.sha256.is_none())
    {
        let key = (recipe.runner.node.clone(), recipe.runner.entry.clone());
        let stamp = if let Some(stamp) = stamps.get(&key) {
            String::clone(stamp)
        } else {
            let stamp = disk::runner_stamp(recipe)?;
            stamps.insert(key, stamp.clone());
            stamp
        };
        recipe.runner.sha256 = Some(stamp);
    }
    Ok(())
}

fn run_members(
    ctx: &Context,
    recipes: Vec<Recipe>,
    action: Action,
    fence: &InputFence,
) -> Result<Value> {
    let mut results = vec![];
    let mut remaining = recipes.into_iter();
    for recipe in remaining.by_ref() {
        let name = recipe.install_name.clone();
        match execute(ctx, recipe, action, false, true, fence.clone()) {
            Ok(value) if value["journal"]["phase"] == "succeeded" => results.push(value),
            Ok(value) => {
                results.push(value);
                return Ok(batch(&results, true, remaining));
            }
            Err(error) if results.is_empty() => return Err(error),
            Err(error) => {
                results.push(json!({"name":name,"error":error}));
                return Ok(batch(&results, true, remaining));
            }
        }
    }
    Ok(batch(&results, false, remaining))
}

fn batch(results: &[Value], incomplete: bool, remaining: impl Iterator<Item = Recipe>) -> Value {
    json!({"backend":"rust","results":results,"incomplete":incomplete,
        "unattempted":remaining.map(|item|item.install_name).collect::<Vec<_>>()})
}

pub(super) struct InstalledSelection {
    pub unowned: Vec<Recipe>,
    pub managed: Vec<Recipe>,
    pub(super) fence: InputFence,
}

pub(super) fn installed_selection(
    ctx: &Context,
    target: &str,
) -> Result<Option<InstalledSelection>> {
    let (witness, raw) = disk::witness(&disk::lock_path(ctx))?;
    let rows = policy::registry(raw.as_deref())?;
    let query = target
        .strip_prefix("source:")
        .or_else(|| target.strip_prefix("skill:"))
        .unwrap_or(target);
    let repository = crate::policy::public_source::repository_subject(query)
        .and_then(|value| crate::policy::source_git::Repository::parse(value).ok());
    let matched: Vec<_> = rows
        .iter()
        .filter(|(name, row)| {
            let Some(source) = public_skill::source_of(row) else {
                return false;
            };
            if let Some(repository) = &repository {
                source == *repository
            } else if query.starts_with("capability:") {
                crate::capability::rules::capability_local::skill_capability_id(
                    &disk::lock_path(ctx),
                    name,
                    "github",
                    source.key(),
                )
                .is_ok_and(|id| id == query)
                    || disk::payload(ctx, name)
                        .and_then(|path| crate::platform::fs::read_path(&path))
                        .and_then(|path| policy::installation_id(&path))
                        .is_ok_and(|id| id == query)
            } else {
                name.as_str() == query
            }
        })
        .collect();
    if matched.is_empty() {
        return Ok(None);
    }
    let (_, book) = load_book(ctx)?;
    let mut unowned = vec![];
    let mut managed = vec![];
    let mut runner = None;
    for (name, row) in matched {
        if let Some(entry) = book
            .recipes
            .iter()
            .find(|entry| entry.install_name == *name)
        {
            // Do not let a new source claim a locally registered name.
            if public_skill::source_of(row)
                .is_none_or(|source| source.key() != entry.source.repository.to_ascii_lowercase())
            {
                return Err(Error::attention(
                    "owner_conflict",
                    "当前技能与此前登记的安装来源不同。请用 mo skill --details 检查。",
                ));
            }
            let installed = policy::installation_id(&crate::platform::fs::read_path(
                &disk::payload(ctx, name)?,
            )?)?;
            if super::owners(ctx, &installed, Action::Adopt)?.1.is_empty() {
                unowned.push(entry.clone());
            } else {
                managed.push(entry.clone());
            }
            continue;
        }
        if runner.is_none() {
            runner = Some(public_skill::runner(ctx)?);
        }
        let runner = runner.clone().ok_or_else(super::invalid)?;
        unowned.push(public_skill::from_record(ctx, name, row, runner)?);
    }
    Ok(Some(InstalledSelection {
        unowned,
        managed,
        fence: InputFence {
            files: vec![witness],
        },
    }))
}

pub(super) fn adopt(ctx: &Context, target: &str, dry: bool, yes: bool) -> Result<Value> {
    native_config_scope(ctx)?;
    try_adopt(ctx, target, dry, yes, false)?.ok_or_else(|| {
        Error::attention("skill_not_found", "本机找不到可接管的 GitHub 技能。").with_guidance(
            "没有匹配的安装。",
            "运行 mo skill --details 检查名称、来源和安装位置。",
        )
    })
}

#[derive(Clone, Copy)]
pub(crate) enum RepositoryAction {
    Install,
    Adopt,
    Upgrade,
}

pub(crate) fn repository_action(ctx: &Context, source: &str) -> Result<RepositoryAction> {
    let request = public_skill::Request::parse(source)?;
    Ok(match installed_selection(ctx, request.repository.key())? {
        None => RepositoryAction::Install,
        Some(selection) if selection.unowned.is_empty() => RepositoryAction::Upgrade,
        Some(_) => RepositoryAction::Adopt,
    })
}

pub(crate) fn upgrade_repository(ctx: &Context, source: &str) -> Result<Value> {
    upgrade_repository_using(ctx, source, |message| {
        crate::operations::confirm(message, false)
    })
}

pub(super) fn upgrade_repository_using(
    ctx: &Context,
    source: &str,
    confirm: impl FnOnce(&str) -> Result<()>,
) -> Result<Value> {
    native_config_scope(ctx)?;
    let request = public_skill::Request::parse(source)?;
    let (commit, recipes, fence) =
        crate::terminal::progress::during("核对目标版本和本机技能", || {
            let selection = installed_selection(ctx, request.repository.key())?
                .filter(|selection| selection.unowned.is_empty())
                .ok_or_else(|| {
                    Error::attention("plan_stale", "本机安装记录已改变。").with_guidance(
                        "尚未开始更新。",
                        format!("运行 mo recommend {} 重新选择。", request.repository.key()),
                    )
                })?;
            let commit = request.resolve()?;
            let mut changes = vec![];
            for recipe in selection.managed {
                verify_managed_skill(ctx, &recipe)?;
                if !policy::generated(&recipe)
                    || recipe.source.commit != commit.as_str()
                    || recipe.source.tracking_ref.is_some()
                {
                    let snapshot = checked_snapshot(ctx, &recipe).map_err(scope_guidance)?;
                    validate_action(ctx, &recipe, Action::Upgrade, &snapshot)?;
                    changes.push(recipe);
                }
            }
            Ok((commit, changes, selection.fence))
        })?;
    if recipes.is_empty() {
        return Ok(json!({"skillAction":"upgrade","status":"up-to-date","source":source}));
    }
    let names = recipes
        .iter()
        .map(|recipe| recipe.install_name.as_str())
        .collect::<Vec<_>>();
    confirm(&format!(
        "更新技能 · {source}\n技能　{}\n位置　{}",
        names.join("、"),
        ctx.home.join(".agents/skills").display()
    ))?;
    crate::operations::recover_before_write(ctx, None)?;
    fence.verify()?;
    let mut result = run_upgrades(ctx, recipes, Some(commit.as_str()), false, true)?;
    result["skillAction"] = json!("upgrade");
    Ok(result)
}

pub(super) fn try_adopt(
    ctx: &Context,
    target: &str,
    dry: bool,
    yes: bool,
    from_upgrade: bool,
) -> Result<Option<Value>> {
    let Some(InstalledSelection {
        unowned: mut recipes,
        fence,
        ..
    }) = installed_selection(ctx, target)?
    else {
        return Ok(None);
    };
    if recipes.is_empty() {
        return Ok((!from_upgrade).then(
            || json!({"skillAction":"adopt","status":"already-managed","persistentWrites":false}),
        ));
    }
    let names = recipes
        .iter()
        .map(|item| item.install_name.clone())
        .collect::<Vec<_>>();
    let pinned_repository = recipes
        .first()
        .filter(|recipe| recipe.source.tracking_ref.is_none())
        .map(|recipe| recipe.source.repository.clone());
    let preflight = || {
        fence.verify()?;
        for recipe in &recipes {
            let snapshot = checked_snapshot(ctx, recipe).map_err(scope_guidance)?;
            validate_action(ctx, recipe, Action::Adopt, &snapshot)?;
        }
        Ok(())
    };
    if dry {
        preflight()?;
    } else {
        crate::terminal::progress::during("核对现有技能和安装位置", preflight)?;
    }
    if dry {
        return Ok(Some(
            json!({"mode":"dry-run","skillAction":"adopt","skills":names,
            "directory":ctx.home.join(".agents/skills"),"persistentWrites":false}),
        ));
    }
    crate::terminal::progress::during("核对安装工具", || seal_runners(&mut recipes))?;
    let note = if from_upgrade && pinned_repository.is_some() {
        "\n保留当前固定版本。接管后可以指定新版本。"
    } else if from_upgrade {
        "\n先接管当前安装，再检查可用更新。"
    } else {
        ""
    };
    crate::operations::confirm(
        &format!(
            "接管现有技能\n技能：{}\n位置：{}\n保留当前安装内容。{note}",
            names.join("、"),
            ctx.home.join(".agents/skills").display()
        ),
        yes,
    )?;
    crate::operations::recover_before_write(ctx, None)?;
    let mut report =
        crate::terminal::progress::during_batch("保存技能的管理记录", || {
            run_members(ctx, recipes, Action::Adopt, &fence)
        })?;
    report["skillAction"] = json!("adopt");
    report["target"] = json!(target);
    if let Some(repository) = pinned_repository {
        report["next"] = json!(format!("mo recommend {repository}"));
    }
    Ok(Some(report))
}

fn scope_guidance(error: Error) -> Error {
    match error.code.as_str() {
        "excluded_path" | "write_symlink_forbidden" => error.with_guidance(
            "技能操作尚未开始。",
            "先在 Agent 的设置中把技能目录设到可管理的位置，再重试。如果这个目录必须保留原样，请继续使用原安装工具。",
        ),
        "source_tree_changed" => error.with_guidance(
            "技能操作尚未开始。",
            "检查上面这个目录的权限和链接。也可以继续用原安装工具维护该技能。",
        ),
        _ => error,
    }
}
