use super::*;
use crate::domain::{Action, Observation};
use crate::{
    domain::Scope,
    policy::chezmoi_native::{Binding, Stamp},
    test_support::TestResult,
};
use clap::Parser;
use std::{os::unix::fs::PermissionsExt, path::PathBuf};

struct Fixture {
    root: tempfile::TempDir,
    context: Context,
    target: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let temporary = crate::platform::fs::read_path(Path::new("/tmp"))?;
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(temporary)?;
        let home = root.path().join("home");
        let source_root = home.join("source");
        fs::create_dir_all(&source_root)?;
        let target = home.join(".demo");
        fs::write(&target, b"fixture\n")?;
        let program = home.join("chezmoi");
        fs::write(&program, b"staging fixture: no process")?;
        let config_root = home.join("native-config");
        fs::create_dir(&config_root)?;
        let config = config_root.join("chezmoi.toml");
        fs::write(&config, "")?;
        let cli = crate::cli::Cli::try_parse_from([
            "macops",
            "--home",
            home.to_str().ok_or("home")?,
            "config",
            "adopt",
            target.to_str().ok_or("target")?,
        ])?;
        let context = Context::from_cli(&cli, &mut vec![])?;
        let store = Store::lock(&context)?;
        let binding = static_binding(
            &context.scope,
            &source_root,
            &config,
            &program,
            &context.state,
        )?;
        io::save_owner(&store, &binding)?;
        let cache = Cache {
            binding,
            entries: std::collections::BTreeMap::new(),
            observed_at: 1,
            complete: true,
            status: vec![],
            source_publication: None,
        };
        Store::atomic(&store.root, CACHE, &cache)?;
        Ok(Self {
            root,
            context,
            target,
            source: source_root.join("dot_demo"),
        })
    }

    fn prepared(&self, store: &Store) -> Result<(Journal, SourceChange)> {
        let ctx = &self.context;
        let data = sources::inspect(ctx, &self.target, SourceMode::Adopt, SourceDelta::None)?;
        let draft = tempfile::Builder::new()
            .prefix(sources::DRAFT_PREFIX)
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(&ctx.state)?;
        let path = draft.path().join(sources::DRAFT_LEAF);
        fs::write(&path, &data.contents)?;
        let proof = SourceChange {
            owner: data.owner,
            target: data.target,
            candidate: sources::file(&path, 256 * 1024)?.1,
            draft_directory: Directory::open(draft.path())?.identity().clone(),
            source_parent: data.parent.path.clone(),
            parent_identity: data.parent.identity().clone(),
            old_cache_sha256: fingerprint(&data.cache)?,
            old_tree_sha256: data.tree_sha256,
        };
        let journal = operations::new_journal(
            ctx,
            plan(ctx, &data.binding, &self.source, &proof, SourceMode::Adopt)?,
        )?;
        let _ = draft.keep();
        store.save(&journal)?;
        Ok((journal, proof))
    }
}

fn static_binding(
    scope: &Scope,
    source: &Path,
    config: &Path,
    program: &Path,
    state: &Path,
) -> Result<Binding> {
    let root = |path: &Path| -> Result<Stamp> {
        let directory = Directory::open(path)?;
        Ok(Stamp {
            path: path.into(),
            identity: Some(directory.identity().clone()),
            mode: directory.file().metadata()?.permissions().mode() & 0o7777,
            digest: fingerprint(&directory.lineage()?)?,
        })
    };
    let mut binding = Binding {
        schema: "chezmoi-owner-v1".into(),
        scope: scope.clone(),
        program: program.into(),
        lookup: program.into(),
        version: "chezmoi version v2.73.3".into(),
        configured_source: source.into(),
        effective_source: source.into(),
        destination: scope.home.clone(),
        config: config.into(),
        cache: scope.home.join("native-cache"),
        persistent_state: scope.home.join("native.db"),
        record_state: state.into(),
        anchors: vec![io::stamp(program, true)?, root(source)?, root(&scope.home)?],
        inputs: vec![],
        effects: "configured-native-v1".into(),
        filtered: false,
    };
    binding.inputs = io::inputs(&binding, &[])?;
    Ok(binding)
}

#[test]
fn source_recovery_distinguishes_unconsumed_candidate_published_inode_and_double_absence()
-> TestResult {
    for (case, expected) in [
        ("old", Some(Phase::Failed)),
        ("new", Some(Phase::Succeeded)),
        ("cache", Some(Phase::Succeeded)),
        ("removed", None),
        ("other", None),
        ("missing-candidate", None),
        ("malformed-cache", None),
    ] {
        let f = Fixture::new()?;
        let ctx = &f.context;
        let store = Store::lock(ctx)?;
        let (mut journal, proof) = f.prepared(&store)?;
        let cache = ctx.state.join(CACHE);
        match case {
            "old" => (),
            "new" | "cache" | "removed" => {
                publish_source(ctx, &store, &journal, &proof)?;
                if case == "cache" {
                    let draft = owned_draft(&journal, &proof)?;
                    publish_cache(ctx, &store, &journal, &proof, &draft)?;
                    fs::remove_dir(&draft.path)?;
                }
                if case != "new" {
                    fs::remove_file(&f.source)?;
                }
            }
            "other" => fs::write(&f.source, fs::read(&f.target)?)?,
            "missing-candidate" => fs::remove_file(&proof.candidate.path)?,
            "malformed-cache" => fs::write(&cache, b"{")?,
            _ => return Err("unexpected case".into()),
        }
        let before = fs::read(&cache)?;
        let operation = ctx.state.join("active.json");
        let old_active = fs::read(&operation)?;
        let result = recover(ctx, &store, &mut journal, &proof, SourceMode::Adopt);
        if let Some(phase) = expected {
            result?;
            assert_eq!(journal.phase, phase, "{case}");
            assert!(!proof.candidate.path.parent().ok_or("draft")?.exists());
        } else {
            let error = result.err().ok_or("accepted unknown recovery")?;
            assert_eq!(error.code, "chezmoi_source_outcome_unknown", "{case}");
            assert_eq!(fs::read(&operation)?, old_active);
            assert_eq!(fs::read(&cache)?, before);
        }
        assert_eq!(fs::read(&f.target)?, b"fixture\n");
    }
    Ok(())
}

#[test]
fn changed_source_slot_fences_publication_even_with_unchanged_target() -> TestResult {
    let f = Fixture::new()?;
    let ctx = &f.context;
    let store = Store::lock(ctx)?;
    let (journal, proof) = f.prepared(&store)?;
    fs::write(&f.source, b"concurrent winner")?;
    assert!(publish_source(ctx, &store, &journal, &proof).is_err());
    assert_eq!(fs::read(&f.source)?, b"concurrent winner");
    assert_eq!(fs::read(&proof.candidate.path)?, b"fixture\n");
    let draft = owned_draft(&journal, &proof)?;
    assert!(publish_cache(ctx, &store, &journal, &proof, &draft).is_err());
    assert_eq!(fs::read(&f.target)?, b"fixture\n");
    Ok(())
}

#[test]
fn source_adoption_plan_rejects_other_actions_paths_and_bad_evidence() -> TestResult {
    let f = Fixture::new()?;
    let store = Store::lock(&f.context)?;
    let (base, _) = f.prepared(&store)?;
    for change in [
        "action",
        "command",
        "target-version",
        "before",
        "target-path",
        "candidate-hash",
        "owner-digest",
        "owner-id",
        "cache-digest",
        "tree-digest",
        "target-digest",
        "target-mode",
        "source-mode",
        "relative-candidate",
        "relative-parent",
        "source-parent",
    ] {
        let mut journal = base.clone();
        let Some(QuerySpec::NativeConfig(ref mut query)) = journal.plan.query else {
            return Err("native query".into());
        };
        let proof = query.source_adopt.as_mut().ok_or("proof")?;
        match change {
            "action" => journal.plan.action = Action::Apply,
            "command" => journal.plan.command = Some(crate::test_support::command("query")),
            "target-version" => journal.plan.target = Some("1".into()),
            "before" => {
                journal.plan.before = Observation::Unknown {
                    reason: "unverified".into(),
                }
            }
            "target-path" => proof.target.path = "/tmp/elsewhere-file".into(),
            "candidate-hash" => proof.candidate.sha256 = "a".repeat(64),
            "owner-digest" => proof.owner.digest = "no".into(),
            "owner-id" => proof.owner.identity = None,
            "cache-digest" => proof.old_cache_sha256 = "not-a-digest".into(),
            "tree-digest" => proof.old_tree_sha256 = "not-a-digest".into(),
            "target-digest" => {
                proof.target.sha256 = "no".into();
                proof.candidate.sha256 = "no".into();
            }
            "target-mode" => proof.target.mode = 0o7777,
            "source-mode" => proof.candidate.mode = 0o7777,
            "relative-candidate" => proof.candidate.path = "relative".into(),
            "relative-parent" => proof.source_parent = "relative".into(),
            "source-parent" => proof.source_parent = "/tmp/other-parent".into(),
            _ => return Err("unexpected mutation".into()),
        }
        assert!(policy::validate_plan(&journal.plan).is_err(), "{change}");
    }
    Ok(())
}

#[test]
fn static_adoption_reads_no_native_credentials_and_rejects_ambiguous_tree_shapes() -> TestResult {
    for case in [
        "already-present",
        "hook",
        "encrypted",
        "symlink",
        "read-dir",
        "relative",
        "owner-permission",
    ] {
        let f = Fixture::new()?;
        let ctx = &f.context;
        let store = Store::lock(ctx)?;
        let mut owner = io::read::<Binding>(ctx, "chezmoi-owner.json")?.ok_or("owner")?;
        let src = owner.effective_source.clone();
        let path = match case {
            "already-present" => {
                fs::write(&f.source, "untracked in cache")?;
                f.target.clone()
            }
            "hook" => {
                fs::write(&owner.config, "[hooks.add.pre]\ncommand='/usr/bin/true'\n")?;
                owner.inputs = io::inputs(&owner, &[])?;
                io::save_owner(&store, &owner)?;
                f.target.clone()
            }
            "encrypted" => {
                fs::write(src.join("encrypted_dot_payload.age"), "blocked")?;
                f.target.clone()
            }
            "symlink" => {
                let alias = ctx.home.join("alias");
                std::os::unix::fs::symlink(&f.target, &alias)?;
                alias
            }
            "read-dir" => ctx.home.join("native-config"),
            "relative" => PathBuf::from("not/absolute"),
            "owner-permission" => {
                fs::set_permissions(f.root.path(), fs::Permissions::from_mode(0o755))?;
                f.target.clone()
            }
            _ => return Err("unexpected fixture".into()),
        };
        let earlier = fs::read(ctx.state.join(CACHE))?;
        assert!(
            sources::inspect(ctx, &path, SourceMode::Adopt, SourceDelta::None).is_err(),
            "{case}"
        );
        assert_eq!(fs::read(ctx.state.join(CACHE))?, earlier);
        assert!(!ctx.state.join("active.json").exists());
    }
    Ok(())
}

#[test]
fn other_config_owners_are_recognized_by_file_identity() -> TestResult {
    for original_owner in ["catalog", "receipt"] {
        let f = Fixture::new()?;
        let ctx = &f.context;
        let alias_dir = ctx.home.join("alias");
        let alias = alias_dir.join(".demo");
        std::os::unix::fs::symlink(&ctx.home, alias_dir)?;
        let mut other = package(&alias, &f.source)?;
        other.provider = Provider::ConfigObserved {
            target: alias,
            source: None,
            authority: crate::domain::ConfigAuthority::Untracked,
        };
        assert_ne!(other.id, crate::policy::config_local::config_id(&f.target)?);
        let store = Store::lock(ctx)?;
        if original_owner == "catalog" {
            fs::create_dir_all(ctx.catalog.parent().ok_or("catalog parent")?)?;
            fs::write(
                &ctx.catalog,
                serde_json::to_vec(&json!({"schema_version":1,"packages":[other]}))?,
            )?;
        } else {
            let legacy = crate::domain::Receipt {
                scope: Some(ctx.scope.clone()),
                query: None,
                package: other,
                observation: Observation::Unknown {
                    reason: "observed metadata".into(),
                },
                operation_id: "OP-existing".into(),
                updated_at: 1,
            };
            let receipts = crate::domain::Receipts {
                schema_version: 1,
                installations: std::collections::BTreeMap::from([(
                    legacy.package.id.clone(),
                    legacy,
                )]),
            };
            Store::atomic(&store.root, "receipts.json", &receipts)?;
        }
        let error = sources::inspect(ctx, &f.target, SourceMode::Adopt, SourceDelta::None)
            .err()
            .ok_or("authorized second owner")?;
        assert_eq!(error.code, "owner_conflict");
        assert!(!ctx.state.join("active.json").exists());
    }
    Ok(())
}

#[test]
fn source_recovery_preserves_state_after_draft_or_publication_metadata_changes() -> TestResult {
    for case in [
        "wrong-draft",
        "draft-mode",
        "different-candidate",
        "changed-marker",
        "wrong-marker",
        "cache-drift",
        "target-drift",
    ] {
        let f = Fixture::new()?;
        let ctx = &f.context;
        let store = Store::lock(ctx)?;
        let (mut journal, proof) = f.prepared(&store)?;
        match case {
            "wrong-draft" => {
                let path = proof.candidate.path.parent().ok_or("draft")?;
                fs::rename(path, ctx.state.join("relocated"))?;
                fs::create_dir(path)?;
            }
            "draft-mode" => {
                let path = proof.candidate.path.parent().ok_or("draft")?;
                fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
            }
            "different-candidate" => fs::write(&proof.candidate.path, b"new raw")?,
            "changed-marker" | "wrong-marker" => {
                publish_source(ctx, &store, &journal, &proof)?;
                let draft = owned_draft(&journal, &proof)?;
                publish_cache(ctx, &store, &journal, &proof, &draft)?;
                let mut cache = io::read::<Cache>(ctx, CACHE)?.ok_or("cache")?;
                let publication = cache.source_publication.as_mut().ok_or("publication")?;
                if case == "changed-marker" {
                    publication.view_sha256 = "b".repeat(64);
                } else {
                    publication.operation_id = "OP-unrelated".into();
                }
                Store::atomic(&store.root, CACHE, &cache)?;
            }
            "cache-drift" => {
                let mut cache = io::read::<Cache>(ctx, CACHE)?.ok_or("cache")?;
                cache.observed_at += 1;
                Store::atomic(&store.root, CACHE, &cache)?;
            }
            "target-drift" => fs::write(&f.target, "new configuration")?,
            _ => return Err("unexpected case".into()),
        }
        let previous_cache = fs::read(ctx.state.join(CACHE))?;
        let previous_record = fs::read(ctx.state.join("active.json"))?;
        let result = recover(ctx, &store, &mut journal, &proof, SourceMode::Adopt);
        assert_eq!(
            result.err().ok_or("accepted altered evidence")?.code,
            "chezmoi_source_outcome_unknown",
            "{case}"
        );
        assert_eq!(fs::read(ctx.state.join(CACHE))?, previous_cache);
        assert_eq!(fs::read(ctx.state.join("active.json"))?, previous_record);
    }
    Ok(())
}

#[test]
fn source_move_never_replaces_a_file_that_appeared_after_the_last_check() -> TestResult {
    let f = Fixture::new()?;
    let ctx = &f.context;
    let store = Store::lock(ctx)?;
    let (journal, proof) = f.prepared(&store)?;
    check_before_source(ctx, &journal, &proof)?;
    let draft = owned_draft(&journal, &proof)?;
    let parent = Directory::open(&proof.source_parent)?;
    fs::write(&f.source, "unrelated new configuration")?;
    let leaf = f.source.file_name().ok_or("source file name")?;
    let error = move_file(&draft, OsStr::new(sources::DRAFT_LEAF), &parent, leaf)
        .err()
        .ok_or("overwrote the live slot")?;
    assert_eq!(error.code, "chezmoi_source_publish_uncertain");
    assert_eq!(fs::read(&f.source)?, b"unrelated new configuration");
    assert_eq!(fs::read(&proof.candidate.path)?, b"fixture\n");
    Ok(())
}

#[test]
fn source_privacy_checks_fail_before_staging_from_a_non_private_state() -> TestResult {
    let f = Fixture::new()?;
    let ctx = &f.context;
    let inputs = sources::inspect(ctx, &f.target, SourceMode::Adopt, SourceDelta::None)?;
    fs::set_permissions(&ctx.state, fs::Permissions::from_mode(0o755))?;
    let error = sources::stage(ctx, &inputs, SourceMode::Adopt)
        .err()
        .ok_or("allowed shared staging")?;
    assert_eq!(error.code, "chezmoi_source_unsupported");
    assert!(!f.source.exists());
    assert!(!ctx.state.join("active.json").exists());
    Ok(())
}

#[test]
fn source_scan_stops_on_unknown_mapping_and_excessive_nesting() -> TestResult {
    for case in [
        "different-entry",
        "nesting",
        "owner-cache",
        "deleted-owner",
        "deleted-cache",
    ] {
        let f = Fixture::new()?;
        let ctx = &f.context;
        let store = Store::lock(ctx)?;
        let mut cache = io::read::<Cache>(ctx, CACHE)?.ok_or("cache")?;
        match case {
            "different-entry" => {
                fs::write(f.source.with_file_name("dot_unrelated"), "different")?;
                cache.entries.insert(
                    ".old".into(),
                    Managed {
                        absolute: ctx.home.join(".old"),
                        source_absolute: f.source.with_file_name("dot_old"),
                        source_relative: PathBuf::from("dot_old"),
                    },
                );
                Store::atomic(&store.root, CACHE, &cache)?;
            }
            "nesting" => {
                let mut deeper = cache.binding.effective_source.clone();
                for _ in 0..25 {
                    deeper.push("nested");
                }
                fs::create_dir_all(deeper)?;
            }
            "owner-cache" => {
                cache.complete = false;
                Store::atomic(&store.root, CACHE, &cache)?;
            }
            "deleted-owner" => fs::remove_file(ctx.state.join("chezmoi-owner.json"))?,
            "deleted-cache" => fs::remove_file(ctx.state.join(CACHE))?,
            _ => return Err("unexpected case".into()),
        }
        assert!(
            sources::inspect(ctx, &f.target, SourceMode::Adopt, SourceDelta::None).is_err(),
            "{case}"
        );
        assert!(!ctx.state.join("active.json").exists());
    }
    Ok(())
}

#[test]
fn source_adopt_reserves_one_mapping_slot_before_it_publishes() -> TestResult {
    let f = Fixture::new()?;
    let ctx = &f.context;
    let store = Store::lock(ctx)?;
    let mut cache = io::read::<Cache>(ctx, CACHE)?.ok_or("cache")?;
    let root = cache.binding.effective_source.clone();
    fs::create_dir(root.join(".git"))?;
    for i in 0..policy::MANAGED_LIMIT - 1 {
        let relative = format!("existing-{i}");
        let source = root.join(&relative);
        fs::write(&source, "original")?;
        cache.entries.insert(
            relative.clone(),
            Managed {
                absolute: ctx.home.join(&relative),
                source_absolute: source,
                source_relative: PathBuf::from(relative),
            },
        );
    }
    Store::atomic(&store.root, CACHE, &cache)?;
    let (mut journal, proof) = f.prepared(&store)?;
    let draft = owned_draft(&journal, &proof)?;
    let parent = Directory::open(&proof.source_parent)?;
    // Recreate the publication window after the rename and before the original writer's sync.
    rustix::fs::renameat_with(
        draft.file(),
        sources::DRAFT_LEAF,
        parent.file(),
        f.source.file_name().ok_or("source file")?,
        rustix::fs::RenameFlags::NOREPLACE,
    )?;
    recover(ctx, &store, &mut journal, &proof, SourceMode::Adopt)?;
    assert_eq!(journal.phase, Phase::Succeeded);
    let cache = io::read::<Cache>(ctx, CACHE)?.ok_or("cache")?;
    assert_eq!(cache.entries.len(), policy::MANAGED_LIMIT);
    let second = ctx.home.join(".next-config");
    fs::write(&second, "next file")?;
    let error = sources::inspect(ctx, &second, SourceMode::Adopt, SourceDelta::None)
        .err()
        .ok_or("admitted an overflowing view")?;
    assert_eq!(error.code, "chezmoi_source_capacity");
    assert!(!root.join("dot_next-config").exists());
    Ok(())
}

#[test]
fn source_adopt_checks_the_depth_of_the_prospective_leaf() -> TestResult {
    let f = Fixture::new()?;
    let ctx = &f.context;
    let store = Store::lock(ctx)?;
    let mut cache = io::read::<Cache>(ctx, CACHE)?.ok_or("cache")?;
    let root = cache.binding.effective_source.clone();
    let mut relative = PathBuf::new();
    for _ in 0..24 {
        relative.push("layer");
        let target = ctx.home.join(&relative);
        let source = root.join(&relative);
        fs::create_dir(&target)?;
        fs::create_dir(&source)?;
        cache.entries.insert(
            relative.to_str().ok_or("relative")?.into(),
            Managed {
                absolute: target,
                source_absolute: source,
                source_relative: relative.clone(),
            },
        );
    }
    Store::atomic(&store.root, CACHE, &cache)?;
    let target = ctx.home.join(relative).join("next");
    fs::write(&target, "new configuration")?;
    let error = sources::inspect(ctx, &target, SourceMode::Adopt, SourceDelta::None)
        .err()
        .ok_or("admitted too much nesting")?;
    assert_eq!(error.code, "chezmoi_source_capacity");
    assert!(!ctx.state.join("active.json").exists());
    Ok(())
}

fn prepare_retirement(f: &Fixture, store: &Store) -> Result<(Journal, SourceChange)> {
    let ctx = &f.context;
    let (mut journal, proof) = f.prepared(store)?;
    publish_source(ctx, store, &journal, &proof)?;
    recover(ctx, store, &mut journal, &proof, SourceMode::Adopt)?;
    fs::write(&f.target, "local edits")?;

    let data = sources::inspect(ctx, &f.target, SourceMode::Forget, SourceDelta::None)?;
    let draft = tempfile::Builder::new()
        .prefix(sources::DRAFT_PREFIX)
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(&ctx.state)?;
    let (_, mut candidate) = sources::file(&f.source, 256 * 1024)?;
    candidate.path = draft.path().join(sources::DRAFT_LEAF);
    let proof = SourceChange {
        owner: data.owner,
        target: data.target,
        candidate,
        draft_directory: Directory::open(draft.path())?.identity().clone(),
        source_parent: data.parent.path.clone(),
        parent_identity: data.parent.identity().clone(),
        old_cache_sha256: fingerprint(&data.cache)?,
        old_tree_sha256: data.tree_sha256,
    };
    let journal = operations::new_journal(
        ctx,
        plan(ctx, &data.binding, &f.source, &proof, SourceMode::Forget)?,
    )?;
    let _ = draft.keep();
    store.save(&journal)?;
    Ok((journal, proof))
}

#[test]
fn source_forget_recovery_preserves_actual_configuration_and_requires_original_retained_object()
-> TestResult {
    for case in ["old", "moved", "cache", "lost", "changed", "winner"] {
        let f = Fixture::new()?;
        let ctx = &f.context;
        let store = Store::lock(ctx)?;
        let (mut journal, proof) = prepare_retirement(&f, &store)?;
        let cache = ctx.state.join(CACHE);
        if case != "old" {
            publish_source(ctx, &store, &journal, &proof)?;
        }
        match case {
            "old" | "moved" => (),
            "cache" => publish_cache(
                ctx,
                &store,
                &journal,
                &proof,
                &owned_draft(&journal, &proof)?,
            )?,
            "lost" => fs::remove_file(&proof.candidate.path)?,
            "changed" => fs::write(&proof.candidate.path, "changed retained object")?,
            "winner" => fs::write(&f.source, "other source owner")?,
            _ => return Err("unknown case".into()),
        }
        let before = fs::read(&cache)?;
        let active = ctx.state.join("active.json");
        let before_active = fs::read(&active)?;
        let result = recover(ctx, &store, &mut journal, &proof, SourceMode::Forget);
        match case {
            "old" => {
                result?;
                assert_eq!(journal.phase, Phase::Failed);
                assert!(f.source.exists());
                assert_eq!(fs::read(&cache)?, before);
            }
            "moved" | "cache" => {
                result?;
                assert_eq!(journal.phase, Phase::Succeeded);
                assert!(
                    io::read::<Cache>(ctx, CACHE)?
                        .ok_or("cache")?
                        .entries
                        .is_empty()
                );
                assert!(!f.source.exists());
                assert!(
                    !proof
                        .candidate
                        .path
                        .parent()
                        .ok_or("retention directory")?
                        .exists()
                );
            }
            _ => {
                assert_eq!(
                    result.err().ok_or("settled uncertain operation")?.code,
                    "chezmoi_source_outcome_unknown"
                );
                assert_eq!(fs::read(&active)?, before_active);
                assert_eq!(fs::read(&cache)?, before);
            }
        }
        assert_eq!(fs::read(&f.target)?, b"local edits");
    }
    Ok(())
}

#[test]
fn source_forget_restores_an_unexpected_file_without_discarding_its_contents() -> TestResult {
    let f = Fixture::new()?;
    let ctx = &f.context;
    let store = Store::lock(ctx)?;
    let (journal, proof) = prepare_retirement(&f, &store)?;
    let (verified, draft) = verify_before_source(ctx, &journal, &proof)?;
    let moved = f.source.with_file_name("external_original");
    fs::rename(&f.source, &moved)?;
    fs::write(&f.source, "another local file")?;
    let error = retire_file(
        &verified.parent,
        f.source.file_name().ok_or("source leaf")?,
        &draft,
        &proof,
    )
    .err()
    .ok_or("accepted unexpected retention")?;
    assert_eq!(error.code, "chezmoi_source_outcome_unknown");
    assert_eq!(fs::read(&f.source)?, b"another local file");
    assert_eq!(fs::read(moved)?, b"fixture\n");
    assert!(!proof.candidate.path.exists());
    Ok(())
}

#[test]
fn native_config_plan_cannot_bind_adopt_and_forget_intents_together() -> TestResult {
    let f = Fixture::new()?;
    let ctx = &f.context;
    let store = Store::lock(ctx)?;
    let (mut journal, proof) = prepare_retirement(&f, &store)?;
    let Some(QuerySpec::NativeConfig(ref mut query)) = journal.plan.query else {
        return Err("native source query".into());
    };
    query.source_adopt = Some(Box::new(proof));
    assert!(policy::validate_plan(&journal.plan).is_err());
    Ok(())
}

#[test]
fn source_forget_recovery_returns_an_unexpected_retained_object_to_its_slot() -> TestResult {
    for scenario in [
        "replacement",
        "changed contents",
        "changed mode",
        "new slot filled",
    ] {
        let f = Fixture::new()?;
        let ctx = &f.context;
        let store = Store::lock(ctx)?;
        let (mut journal, proof) = prepare_retirement(&f, &store)?;
        let (verified, draft) = verify_before_source(ctx, &journal, &proof)?;
        let original = f.source.with_file_name("outside-original");
        if matches!(scenario, "replacement" | "new slot filled") {
            fs::rename(&f.source, &original)?;
            fs::write(&f.source, "concurrent source")?;
        }
        move_file(
            &verified.parent,
            f.source.file_name().ok_or("source leaf")?,
            &draft,
            OsStr::new(sources::DRAFT_LEAF),
        )?;
        if scenario == "changed contents" {
            fs::write(&proof.candidate.path, "saved while open")?;
        }
        if scenario == "changed mode" {
            let different = if proof.candidate.mode == 0o600 {
                0o644
            } else {
                0o600
            };
            fs::set_permissions(&proof.candidate.path, fs::Permissions::from_mode(different))?;
        }
        if scenario == "new slot filled" {
            fs::write(&f.source, "later configuration")?;
        }
        let before = fs::read(ctx.state.join(CACHE))?;
        let before_id = sources::file(&proof.candidate.path, 256 * 1024)?.1;
        let error = recover(ctx, &store, &mut journal, &proof, SourceMode::Forget)
            .err()
            .ok_or("settled a changed source")?;
        assert_eq!(error.code, "chezmoi_source_outcome_unknown");
        if scenario == "new slot filled" {
            assert_eq!(fs::read(&f.source)?, b"later configuration");
            assert_eq!(
                sources::file(&proof.candidate.path, 256 * 1024)?.1,
                before_id
            );
        } else {
            let mut returned = sources::file(&f.source, 256 * 1024)?.1;
            returned.path.clone_from(&proof.candidate.path);
            assert_eq!(returned, before_id);
            assert!(!proof.candidate.path.exists());
        }
        if original.exists() {
            assert_eq!(fs::read(&original)?, b"fixture\n");
        }
        assert_eq!(journal.phase, Phase::Prepared);
        assert_eq!(fs::read(ctx.state.join(CACHE))?, before);
        assert_eq!(fs::read(&f.target)?, b"local edits");
    }
    Ok(())
}

#[test]
fn source_admission_requires_the_expected_tool_and_static_configuration_shape() -> TestResult {
    for (case, body, accepted) in [
        ("plain-version", "sourceDir='/tmp/documents'\n", true),
        ("bad-value", "sourceDir=[]\n", false),
        ("invalid-toml", "key = [", false),
        ("binary", "", false),
        ("unsupported-format", "", false),
        ("unsupported-version", "", false),
    ] {
        let f = Fixture::new()?;
        let ctx = &f.context;
        let store = Store::lock(ctx)?;
        let mut cache = io::read::<Cache>(ctx, CACHE)?.ok_or("cache")?;
        let mut binding = cache.binding.clone();
        if case == "unsupported-format" {
            binding.config.set_extension("yaml");
        }
        if case == "unsupported-version" {
            binding.version = "chezmoi version v2.74.1".into();
        }
        if case == "binary" {
            fs::write(&binding.config, [0xff, 0xfe])?;
        } else {
            fs::write(&binding.config, body)?;
        }
        binding.inputs = io::inputs(&binding, &[])?;
        io::save_owner(&store, &binding)?;
        cache.binding = binding;
        Store::atomic(&store.root, CACHE, &cache)?;
        let status = sources::inspect(ctx, &f.target, SourceMode::Adopt, SourceDelta::None);
        assert_eq!(status.is_ok(), accepted, "{case}");
        assert!(!f.source.exists());
        assert!(!ctx.state.join("active.json").exists());
    }
    Ok(())
}

#[test]
fn system_home_classification_only_reads_the_account_location() -> TestResult {
    let f = Fixture::new()?;
    let uid = nix::unistd::geteuid();
    let user = nix::unistd::User::from_uid(uid)?.ok_or("local account")?;
    let mut ctx = f.context.clone();
    ctx.home = crate::platform::fs::read_path(&user.dir)?;
    ctx.state = ctx.home.join(".local/state/macops");
    assert!(!sources::check_scope(&ctx)?);
    Ok(())
}

#[test]
fn source_forget_repair_preserves_retention_when_cache_owner_or_root_changes() -> TestResult {
    for case in ["owner", "cache", "root"] {
        let f = Fixture::new()?;
        let ctx = &f.context;
        let store = Store::lock(ctx)?;
        let (mut journal, proof) = prepare_retirement(&f, &store)?;
        let (verified, draft) = verify_before_source(ctx, &journal, &proof)?;
        let original = ctx.home.join("external-original");
        fs::rename(&f.source, &original)?;
        fs::write(&f.source, "unrelated incoming data")?;
        move_file(
            &verified.parent,
            f.source.file_name().ok_or("source leaf")?,
            &draft,
            OsStr::new(sources::DRAFT_LEAF),
        )?;
        match case {
            "owner" => {
                let name = ctx.state.join("chezmoi-owner.json");
                let underlying = fs::read(&name)?;
                fs::rename(&name, ctx.state.join("old-owner.json"))?;
                fs::write(name, underlying)?;
            }
            "cache" => {
                let mut cache = io::read::<Cache>(ctx, CACHE)?.ok_or("cache")?;
                cache.observed_at += 1;
                Store::atomic(&store.root, CACHE, &cache)?;
            }
            "root" => {
                let root = &proof.source_parent;
                fs::rename(root, ctx.home.join("rotated"))?;
                fs::create_dir(root)?;
            }
            _ => return Err("unknown case".into()),
        }
        let before_cache = fs::read(ctx.state.join(CACHE))?;
        let before_active = fs::read(ctx.state.join("active.json"))?;
        let error = recover(ctx, &store, &mut journal, &proof, SourceMode::Forget)
            .err()
            .ok_or("restored through changed namespace")?;
        assert_eq!(error.code, "chezmoi_source_outcome_unknown");
        assert_eq!(fs::read(&proof.candidate.path)?, b"unrelated incoming data");
        assert_eq!(fs::read(original)?, b"fixture\n");
        assert_eq!(fs::read(&f.target)?, b"local edits");
        assert!(!f.source.exists());
        assert_eq!(fs::read(ctx.state.join(CACHE))?, before_cache);
        assert_eq!(fs::read(ctx.state.join("active.json"))?, before_active);
    }
    Ok(())
}

#[test]
fn terminal_forget_can_resume_cleanup_without_discarding_a_different_object() -> TestResult {
    for replacement in [false, true] {
        let f = Fixture::new()?;
        let ctx = &f.context;
        let store = Store::lock(ctx)?;
        let (mut journal, proof) = prepare_retirement(&f, &store)?;
        publish_source(ctx, &store, &journal, &proof)?;
        let draft = owned_draft(&journal, &proof)?;
        publish_cache(ctx, &store, &journal, &proof, &draft)?;
        journal.phase = Phase::Succeeded;
        store.save(&journal)?;
        if replacement {
            fs::rename(&proof.candidate.path, ctx.home.join("retained-external"))?;
            fs::write(&proof.candidate.path, "other contents")?;
        }
        let before_cache = fs::read(ctx.state.join(CACHE))?;
        let before_active = fs::read(ctx.state.join("active.json"))?;
        recover(ctx, &store, &mut journal, &proof, SourceMode::Forget)?;
        if replacement {
            assert_eq!(fs::read(&proof.candidate.path)?, b"other contents");
        } else {
            assert!(!draft.path.exists());
        }
        assert!(!f.source.exists());
        assert_eq!(fs::read(f.target)?, b"local edits");
        assert_eq!(fs::read(ctx.state.join(CACHE))?, before_cache);
        assert_eq!(fs::read(ctx.state.join("active.json"))?, before_active);
    }
    Ok(())
}

#[test]
fn source_recovery_requires_its_original_home_state_and_process_free_journal() -> TestResult {
    let f = Fixture::new()?;
    let alternate = Fixture::new()?;
    let ctx = &f.context;
    let store = Store::lock(ctx)?;
    let (original, proof) = prepare_retirement(&f, &store)?;
    let before = fs::read(ctx.state.join("active.json"))?;
    for case in ["home", "state", "process"] {
        let mut journal = original.clone();
        let mut context = ctx;
        match case {
            "home" => context = &alternate.context,
            "state" => journal.state = ctx.state.join("other"),
            "process" => journal.process_group = Some(42),
            _ => return Err("unexpected case".into()),
        }
        let error = recover(context, &store, &mut journal, &proof, SourceMode::Forget)
            .err()
            .ok_or("recovered without the original scope")?;
        assert_eq!(error.code, "chezmoi_source_outcome_unknown");
        assert_eq!(fs::read(ctx.state.join("active.json"))?, before);
        assert!(f.source.exists());
    }
    Ok(())
}
