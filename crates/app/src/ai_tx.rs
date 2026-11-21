use crate::{
    ai_gate as gate,
    ai_state::{self, Snapshot, Step, Upgrade},
    config_tx,
    context::Context,
    domain::{CommandSpec, Journal, Phase, Plan, fingerprint},
    error::{Error, Result},
    operations,
    paths::Directory,
    policy::ai as policy,
    process,
    store::Store,
};
use serde_json::{Value, json};
use std::{ffi::OsStr, path::Path};

pub fn is_native(journal: &Journal) -> bool {
    journal.plan.package.id.starts_with("ai-native:")
        && journal
            .plan
            .warnings
            .first()
            .is_some_and(|warning| warning.starts_with("ai-native-v1:"))
}
fn result(journal: &Journal) -> Value {
    json!({"backend":"rust", "journal":journal})
}
fn program(journal: &Journal) -> Result<&Path> {
    journal
        .plan
        .command
        .as_ref()
        .map(|s| s.program.as_path())
        .ok_or_else(|| Error::attention("ai_plan_invalid", "升级命令缺失"))
}
fn frozen_context(journal: &Journal, upgrade: &Upgrade) -> Context {
    Context {
        home: journal.home.clone(),
        scope: journal.scope.clone(),
        state: journal.state.clone(),
        cache: journal.cache.clone(),
        settings: journal.settings.clone(),
        config: journal.home.join(".config/macops"),
        data: journal.home.join(".local/share/macops"),
        catalog: journal.home.join(".config/macops/catalog.json"),
        catalog_explicit: false,
        timeout_secs: journal.plan.timeout_secs,
        isolated: upgrade.isolated,
        harness: crate::context::HarnessRoots::default(),
        uv: crate::uv_local::DiscoveryInputs::default(),
        chezmoi: upgrade.mise.as_ref().map_or_else(
            || crate::config_inventory::Inputs {
                source: None,
                destination: None,
                config: None,
                env: std::collections::BTreeMap::new(),
            },
            |m| m.roots.clone(),
        ),
    }
}
fn verify_inputs(ctx: &Context, upgrade: &Upgrade) -> Result<()> {
    ai_state::verify_profiles(&upgrade.profiles)?;
    if let Some(mise) = &upgrade.mise {
        if mise.live.current()? != mise.live
            || mise.source.current()? != mise.source
            || config_tx::root_digest(ctx)? != mise.root_digest
        {
            return Err(Error::attention(
                "plan_stale",
                "mise 配置、源或根解析在准备期间改变",
            ));
        }
        policy::require_sync(&mise.live.text, &mise.source.text, upgrade.force_live)?;
        config_tx::check_mise_layers(&mise.live.anchor.canonical_parent)?;
    }
    Ok(())
}

pub fn execute(ctx: &Context, upgrade: &Upgrade, plan: Plan) -> Result<Value> {
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    verify_inputs(ctx, upgrade)?;
    let mut journal = operations::new_journal(ctx, plan)?;
    ai_state::save(&store, &journal, upgrade)?;
    ai_state::step(&store, &journal, Step::Inspect)?;
    store.save(&journal)?;
    let output = operations::launch(&store, &mut journal)?;
    if output.code != Some(0)
        || output.uncertain
        || journal.process_group.is_some_and(process::group_alive)
    {
        journal.phase = Phase::Unknown;
        journal.message = "升级器结果未知；先 recover 核验，不会重新运行升级器".into();
        store.save(&journal)?;
        return Ok(result(&journal));
    }
    Store::atomic(
        &store.root,
        &format!("ai-{}-exit.json", journal.id),
        &(output.code == Some(0)),
    )?;
    finish(ctx, &store, &mut journal, upgrade)
}

pub fn recover(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    resolve_failed: bool,
    yes: bool,
) -> Result<Value> {
    let upgrade = ai_state::read(store, journal)?;
    if matches!(journal.phase, Phase::Succeeded | Phase::Failed) {
        return Ok(result(journal));
    }
    if journal.process_group.is_some_and(process::group_alive) {
        return Err(Error::attention(
            "operation_still_running",
            "原进程组仍在运行，拒绝恢复或重新执行",
        ));
    }
    if resolve_failed {
        operations::confirm(
            "确认已人工核对升级器、载荷、配置和源；仅记录失败解除阻断，不会重跑或修复。",
            yes,
        )?;
        journal.phase = Phase::Failed;
        journal.message = "人工核对后记录失败；没有重新运行升级器或回滚".into();
        store.save(journal)?;
        return Ok(result(journal));
    }
    if journal.phase == Phase::Prepared && journal.process_group.is_none() {
        journal.phase = Phase::Failed;
        journal.message = "未发送升级执行 gate".into();
        store.save(journal)?;
        return Ok(result(journal));
    }
    if journal.process_group.is_none() {
        return Err(Error::attention(
            "manual_recovery_required",
            "不能证明原升级器已结束；保留阻断",
        ));
    }
    let frozen = frozen_context(journal, &upgrade);
    if ctx.state != journal.state || ctx.home != journal.home {
        return Err(Error::attention(
            "ai_scope_changed",
            "恢复作用域与原操作不同",
        ));
    }
    finish(&frozen, store, journal, &upgrade)
}

fn finish(ctx: &Context, store: &Store, journal: &mut Journal, upgrade: &Upgrade) -> Result<Value> {
    let outcome = store
        .verify_namespace()
        .and_then(|()| continue_upgrade(ctx, store, journal, upgrade));
    if let Err(error) = outcome {
        journal.phase = Phase::Unknown;
        journal.message = format!("{}；保留阻断，先核对配置及原操作", error.code);
    }
    store.save(journal)?;
    Ok(result(journal))
}
fn complete(journal: &mut Journal, succeeded: bool) {
    journal.phase = if succeeded {
        Phase::Succeeded
    } else {
        Phase::Failed
    };
    journal.message = if succeeded {
        "原生 AI 升级通过版本、载荷与配置核验"
    } else {
        "载荷门禁失败；已精确回退升级前本机 pin 并完成 reshim，源未推进"
    }
    .into();
}

fn continue_upgrade(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    upgrade: &Upgrade,
) -> Result<()> {
    ai_state::verify_profiles(&upgrade.profiles)?;
    let step = ai_state::read_step(store, journal)?;
    let Some(mise) = &upgrade.mise else {
        let observed = gate::version(ctx, program(journal)?, "mimo")?;
        if upgrade.version.as_ref().is_some_and(|v| v != &observed) {
            return Err(Error::attention(
                "ai_version_mismatch",
                "mimo 实际版本与目标不同",
            ));
        }
        let clean: Option<bool> =
            Store::read(&store.root, &format!("ai-{}-exit.json", journal.id))?;
        if upgrade.version.is_none()
            && clean != Some(true)
            && journal.plan.before.version() == Some(observed.as_str())
        {
            return Err(Error::attention(
                "ai_vendor_result_unknown",
                "厂商升级结果未知且版本未改变",
            ));
        }
        complete(journal, true);
        return Ok(());
    };
    config_tx::check_mise_layers(&mise.live.anchor.canonical_parent)?;
    if config_tx::root_digest(ctx)? != mise.root_digest {
        return Err(Error::attention(
            "ai_roots_changed",
            "mise 源根解析发生变化",
        ));
    }
    match step {
        Step::Inspect => inspect(ctx, store, journal, upgrade),
        Step::Rollback => rollback(store, journal, upgrade),
        Step::Reshim => reshim(store, journal, upgrade),
        Step::Reconcile => reconcile(ctx, store, journal, upgrade),
    }
}
fn inspect(ctx: &Context, store: &Store, journal: &mut Journal, upgrade: &Upgrade) -> Result<()> {
    let mise = upgrade
        .mise
        .as_ref()
        .ok_or_else(|| Error::new("ai_plan_invalid", "缺少 mise 输入"))?;
    let current = mise.live.current()?;
    if mise.source.current()? != mise.source {
        return Err(Error::attention("ai_source_changed", "源在升级期间改变"));
    }
    if current.text != mise.expected_live {
        return Err(Error::attention(
            "ai_live_changed",
            "升级后的整份本机配置不是预期单键变化",
        ));
    }
    let target = upgrade
        .version
        .as_deref()
        .ok_or_else(|| Error::new("ai_plan_invalid", "缺少目标版本"))?;
    let gate = gate::payload(ctx, program(journal)?, &upgrade.tool, target).and_then(|evidence| {
        ai_state::verify_profiles(&upgrade.profiles)?;
        Ok(evidence)
    });
    let evidence = match gate {
        Ok(evidence) => evidence,
        Err(error) if error.code == "ai_payload_changed" => return Err(error),
        Err(_) => {
            Store::atomic_new(
                &store.root,
                &format!("ai-{}-post.json", journal.id),
                &current,
            )?;
            ai_state::step(store, journal, Step::Rollback)?;
            return rollback(store, journal, upgrade);
        }
    };
    if upgrade.force_live {
        evidence.verify()?;
        verify_probe_fence(ctx, store, upgrade, &current, &mise.source)?;
        complete(journal, true);
        return Ok(());
    }
    ai_state::step(store, journal, Step::Reconcile)?;
    reconcile(ctx, store, journal, upgrade)
}

fn verify_probe_fence(
    ctx: &Context,
    store: &Store,
    upgrade: &Upgrade,
    live: &Snapshot,
    source: &Snapshot,
) -> Result<()> {
    if live.current()? != *live || source.current()? != *source {
        return Err(Error::attention(
            "ai_gate_inputs_changed",
            "载荷探测期间本机配置或源印记改变，保留外部内容与阻断",
        ));
    }
    let mise = upgrade
        .mise
        .as_ref()
        .ok_or_else(|| Error::new("ai_plan_invalid", "缺少 mise 输入"))?;
    config_tx::check_mise_layers(&mise.live.anchor.canonical_parent)?;
    if config_tx::root_digest(ctx)? != mise.root_digest {
        return Err(Error::attention(
            "ai_roots_changed",
            "载荷探测期间配置根解析改变",
        ));
    }
    ai_state::verify_profiles(&upgrade.profiles)?;
    store.verify_namespace()
}

fn rollback(store: &Store, journal: &mut Journal, upgrade: &Upgrade) -> Result<()> {
    let mise = upgrade
        .mise
        .as_ref()
        .ok_or_else(|| Error::new("ai_plan_invalid", "缺少 mise 输入"))?;
    let post: Snapshot = ai_state::snapshot(store, &format!("ai-{}-post.json", journal.id))?
        .ok_or_else(|| Error::attention("ai_rollback_unknown", "缺少回退前态印记"))?;
    let candidate_name = format!("ai-{}-rollback.json", journal.id);
    let candidate: Option<Snapshot> = ai_state::snapshot(store, &candidate_name)?;
    let current = mise.live.current()?;
    if candidate.as_ref() != Some(&current) {
        if current != post || mise.source.current()? != mise.source {
            return Err(Error::attention(
                "ai_rollback_cas_failed",
                "本机配置或源被并发修改，拒绝覆盖",
            ));
        }
        let directory = Directory::open(&post.anchor.canonical_parent)?;
        directory.atomic_replace_regular_verified(
            OsStr::new(&post.anchor.name),
            mise.live.text.as_bytes(),
            mise.live.mode,
            |_, meta| {
                if mise.live.current()? != post || mise.source.current()? != mise.source {
                    return Err(Error::attention(
                        "ai_rollback_cas_failed",
                        "回退前 CAS 不匹配",
                    ));
                }
                let candidate = Snapshot {
                    identity: meta.identity.clone(),
                    text: mise.live.text.clone(),
                    mode: meta.mode,
                    ..post.clone()
                };
                Store::atomic(&store.root, &candidate_name, &candidate)?;
                store.verify_namespace()
            },
        )?;
    }
    let candidate: Snapshot = ai_state::snapshot(store, &candidate_name)?
        .ok_or_else(|| Error::attention("ai_rollback_unknown", "回退候选印记缺失"))?;
    if mise.live.current()? != candidate
        || policy::pin(&candidate.text, &upgrade.tool)? != mise.prev_live_pin
    {
        return Err(Error::attention(
            "ai_rollback_cas_failed",
            "回退结果不匹配原本机版本",
        ));
    }
    ai_state::verify_profiles(&upgrade.profiles)?;
    store.verify_namespace()?;
    ai_state::step(store, journal, Step::Reshim)?;
    reshim(store, journal, upgrade)
}
fn reshim(store: &Store, journal: &mut Journal, upgrade: &Upgrade) -> Result<()> {
    let mise = upgrade
        .mise
        .as_ref()
        .ok_or_else(|| Error::new("ai_plan_invalid", "缺少 mise 输入"))?;
    let expected: Snapshot =
        ai_state::snapshot(store, &format!("ai-{}-rollback.json", journal.id))?
            .ok_or_else(|| Error::attention("ai_rollback_unknown", "缺少回退结果印记"))?;
    if mise.live.current()? != expected || mise.source.current()? != mise.source {
        return Err(Error::attention(
            "ai_rollback_cas_failed",
            "reshim 前配置或源改变",
        ));
    }
    let output = operations::launch(store, journal)?;
    if output.code != Some(0)
        || output.uncertain
        || journal.process_group.is_some_and(process::group_alive)
    {
        return Err(Error::attention(
            "ai_rollback_failed",
            "reshim 未完成或失败",
        ));
    }
    if mise.live.current()? != expected || mise.source.current()? != mise.source {
        return Err(Error::attention(
            "ai_rollback_cas_failed",
            "reshim 后配置或源改变",
        ));
    }
    ai_state::verify_profiles(&upgrade.profiles)?;
    complete(journal, false);
    Ok(())
}
fn reconcile(ctx: &Context, store: &Store, journal: &mut Journal, upgrade: &Upgrade) -> Result<()> {
    let mise = upgrade
        .mise
        .as_ref()
        .ok_or_else(|| Error::new("ai_plan_invalid", "缺少 mise 输入"))?;
    let live = mise.live.current()?;
    let source = mise.source.current()?;
    if live.text != mise.expected_live {
        return Err(Error::attention("ai_live_changed", "源写回前本机配置改变"));
    }
    let target = upgrade
        .version
        .as_deref()
        .ok_or_else(|| Error::new("ai_plan_invalid", "缺少目标版本"))?;
    let evidence = gate::payload(ctx, program(journal)?, &upgrade.tool, target)?;
    evidence.verify()?;
    verify_probe_fence(ctx, store, upgrade, &live, &source)?;
    if source.text == mise.expected_source && source != mise.source {
        config_tx::ai_reconcile_completed(store, journal)?;
        complete(journal, true);
        return Ok(());
    }
    if source != mise.source {
        return Err(Error::attention("ai_source_changed", "源写回前并发修改"));
    }
    if source.text == mise.expected_source {
        complete(journal, true);
        return Ok(());
    }
    let fence =
        config_tx::capture_reconcile_fence(ctx, &live.anchor.requested, &source.anchor.requested)?;
    let spec = crate::policy::config_tx::ai_mise_spec(&upgrade.tool)?;
    let from = policy::pin(&source.text, &upgrade.tool)?;
    let to = upgrade
        .version
        .as_deref()
        .ok_or_else(|| Error::new("ai_plan_invalid", "缺少目标版本"))?;
    store.verify_namespace()?;
    let verify_payload = || evidence.verify();
    config_tx::reconcile_source_locked(
        store,
        ctx,
        &config_tx::ReconcileWrite {
            guard: Some(&verify_payload),
            operation: Some(journal),
            live_path: &live.anchor.requested,
            src_path: &source.anchor.requested,
            spec,
            from: &from,
            to,
            rewritten: &mise.expected_source,
            fence: &fence,
        },
    )?;
    ai_state::verify_profiles(&upgrade.profiles)?;
    config_tx::ai_reconcile_completed(store, journal)?;
    evidence.verify()?;
    complete(journal, true);
    Ok(())
}

pub fn worker_command(journal: &Journal) -> Result<Option<CommandSpec>> {
    if !is_native(journal) {
        return Ok(None);
    }
    let upgrade: Upgrade =
        crate::paths::regular_json(&journal.state.join(format!("ai-{}.json", journal.id)))?;
    if journal.plan.warnings.first() != Some(&format!("ai-native-v1:{}", fingerprint(&upgrade)?)) {
        return Err(Error::attention("ai_plan_changed", "AI 输入与冻结摘要不同"));
    }
    let step: Step =
        crate::paths::regular_json(&journal.state.join(format!("ai-{}-step.json", journal.id)))?;
    if step == Step::Reshim {
        return Ok(Some(CommandSpec {
            program: program(journal)?.into(),
            args: vec!["reshim".into()],
        }));
    }
    if step != Step::Inspect {
        return Err(Error::attention("ai_worker_step", "此阶段不允许运行升级器"));
    }
    verify_inputs(&frozen_context(journal, &upgrade), &upgrade)?;
    Ok(journal.plan.command.clone())
}

#[cfg(test)]
#[path = "../tests/unit/ai_worker.rs"]
mod worker_tests;
