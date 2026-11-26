use crate::{
    capability::{
        capability_guard, capability_store as store,
        link_inventory::Reader,
        rules::{
            delivery::{self, Profile, Profiles},
            invocation,
        },
    },
    context::Context,
    error::{Error, Result},
    operations::governance::Governance,
    platform::fs::Directory,
    policy::{
        governance::Dataset,
        profile::{
            claude_fields, codex_profile,
            profile_authority::{self, Authority, Client, StaticSource, Target},
            profile_fields::{ConfigProfile, Intent, Owner, Role},
            profile_registry,
            profile_select::Selected,
        },
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
fn invalid() -> Error {
    Error::attention(
        "unverified_profile_configuration",
        "profile、插件登记或配置来源无法完整验证",
    )
}
pub struct Patch {
    pub path: PathBuf,
    pub before: crate::capability::rules::capability_tx::State,
    pub namespace: crate::capability::rules::capability_tx::Namespace,
    pub before_bytes: Vec<u8>,
    pub after: Vec<u8>,
    pub mode: u32,
}
pub struct Assessment {
    pub reader: Reader,
    pub authority: Authority,
    pub patches: Vec<Patch>,
    pub book_before: Option<Vec<u8>>,
    pub book_after: Option<Vec<u8>>,
    pub pending: Vec<Value>,
}
impl Assessment {
    pub fn verify(&self) -> Result<()> {
        self.reader.verify()
    }
}
fn dataset(reader: &mut Reader, authority: &Governance, key: Dataset) -> Result<Option<Vec<u8>>> {
    let raw = authority.read(key, 16 * 1024 * 1024)?;
    if reader.regular(&authority.paths.dataset(key))? != raw {
        return Err(invalid());
    }
    Ok(raw)
}
fn capture_tree(reader: &mut Reader, path: &Path, depth: usize) -> Result<()> {
    if depth > 16 {
        return Err(invalid());
    }
    let Some(root) = reader.directory(path)? else {
        return Ok(());
    };
    for (name, entry) in capability_guard::listing(&root)? {
        if entry.kind == "directory" && !name.starts_with('.') {
            capture_tree(reader, &path.join(name), depth + 1)?;
        } else if name.starts_with(".chezmoi") {
            reader.regular(&path.join(name))?;
        }
    }
    Ok(())
}
fn capture_configured_root(
    reader: &mut Reader,
    roots: &crate::config::config_inventory::Roots,
) -> Result<()> {
    let root = reader
        .directory(&roots.configured_source)?
        .ok_or_else(invalid)?;
    for (name, _) in capability_guard::listing(&root)? {
        if name.starts_with(".chezmoi") && name != ".chezmoiroot" {
            return Err(Error::attention(
                "profile_config_dynamic_source",
                "配置根含未求值的控制文件，未取得字段写权",
            ));
        }
    }
    reader.regular(&roots.configured_source.join(".chezmoiroot"))?;
    Ok(())
}
struct Mapping {
    root: Option<crate::config::config_inventory::Roots>,
    entries: BTreeMap<PathBuf, PathBuf>,
    unmanaged: bool,
}
fn mapping(ctx: &Context, reader: &mut Reader) -> Result<Mapping> {
    let (candidates, selected_config) = crate::config::config_inventory::lookup_candidates(ctx)?;
    for path in candidates {
        reader.regular(&path)?;
    }
    let config_bytes = selected_config
        .as_ref()
        .map(|p| reader.regular(p))
        .transpose()?
        .flatten();
    let configuration = selected_config.as_deref().zip(config_bytes.as_deref());
    let (sources, selected_source) =
        crate::config::config_inventory::source_lookup(ctx, configuration)?;
    for source in sources {
        reader.anchor(&source)?;
    }
    let snapshot = crate::config::config_inventory::discover(ctx);
    let discovered_config = snapshot
        .roots
        .as_ref()
        .and_then(|r| r.config.clone())
        .or_else(|| snapshot.coverage.roots.get(1).cloned());
    if discovered_config != selected_config {
        return Err(invalid());
    }
    if snapshot.coverage.roots.first() != Some(&selected_source) {
        return Err(invalid());
    }
    if let Some(destination) = &snapshot.destination {
        destination.verify()?;
    }
    if snapshot.coverage.state == crate::domain::resource::CoverageState::Absent {
        for (index, root) in snapshot.coverage.roots.iter().enumerate() {
            if index == 0 {
                reader.anchor(root)?;
            } else {
                reader.regular(root)?;
            }
        }
        return Ok(Mapping {
            root: None,
            entries: BTreeMap::new(),
            unmanaged: true,
        });
    }
    let roots = snapshot.roots.ok_or_else(invalid)?;
    let complete = snapshot
        .coverage
        .issues
        .iter()
        .all(|i| i == "physical_candidates_only");
    if !complete {
        return Err(Error::attention(
            "profile_config_dynamic_source",
            "配置源存在动态或不完整范围，未取得 profile 字段写权",
        ));
    }
    capture_configured_root(reader, &roots)?;
    capture_tree(reader, &roots.effective_source, 0)?;
    if let Some(config) = &roots.config {
        reader.regular(config)?;
    }
    let mut entries = BTreeMap::new();
    for row in snapshot.candidates {
        if entries.insert(row.target, row.source.path).is_some() {
            return Err(invalid());
        }
    }
    Ok(Mapping {
        root: Some(roots),
        entries,
        unmanaged: true,
    })
}
fn pstack_catalog(
    reader: &mut Reader,
    root: &Path,
    allowed: &BTreeSet<String>,
) -> Result<(Vec<String>, String)> {
    let path = root.join("plugins/installed_plugins.json");
    let bytes = reader.regular(&path)?.ok_or_else(invalid)?;
    let book = crate::policy::catalog::resources::plugin_book(&bytes)?;
    let rows: Vec<_> = book
        .plugins
        .get("pstack@cursor-plugins")
        .into_iter()
        .flatten()
        .filter(|r| r.scope == "user")
        .collect();
    let [record] = rows.as_slice() else {
        return Err(invalid());
    };
    let version = record.version.as_deref().ok_or_else(invalid)?;
    if !crate::policy::catalog::resources::component(version)
        || record.path
            != root
                .join("plugins/cache/cursor-plugins/pstack")
                .join(version)
    {
        return Err(invalid());
    }
    let payload = reader.directory(&record.path)?.ok_or_else(invalid)?;
    let skills = reader
        .directory(&payload.path.join("skills"))?
        .ok_or_else(invalid)?;
    let mut names = BTreeSet::new();
    for (name, entry) in capability_guard::listing(&skills)? {
        if entry.kind != "directory" {
            return Err(invalid());
        }
        delivery::LinkText::managed(&name)?;
        if reader
            .regular(&skills.path.join(&name).join("SKILL.md"))?
            .is_none()
        {
            return Err(invalid());
        }
        names.insert(name);
    }
    if names.is_empty() || !allowed.is_subset(&names) {
        return Err(invalid());
    }
    let evidence = invocation::hash(&serde_json::to_vec(&(
        &record.path,
        &record.version,
        &record.revision,
        &names,
    ))?);
    Ok((
        names
            .difference(allowed)
            .map(|name| format!("Skill(pstack:{name})"))
            .collect(),
        evidence,
    ))
}
#[derive(Clone)]
enum Desired {
    Claude(claude_fields::Request),
    Codex(codex_profile::Request),
}
struct Work {
    clients: BTreeMap<String, Client>,
    desired: Vec<Desired>,
}
fn add_target(
    work: &mut BTreeMap<PathBuf, Work>,
    path: PathBuf,
    client: Client,
    mut desired: Desired,
) -> Result<()> {
    bind_intent(&mut desired, &client)?;
    let id = client.owner.id();
    let row = work.entry(path).or_insert_with(|| Work {
        clients: BTreeMap::new(),
        desired: vec![],
    });
    if row.clients.insert(id, client).is_some() {
        return Err(invalid());
    }
    row.desired.push(desired);
    Ok(())
}
fn bind_intent(desired: &mut Desired, client: &Client) -> Result<()> {
    let (hash, fields) = match desired {
        Desired::Claude(request) => {
            let fields = match &request.desired {
                Some(claude_fields::Desired::Plugins(ids)) => json!({"plugins_off":ids}),
                Some(claude_fields::Desired::Pstack(entries)) => json!({"pstack_deny":entries}),
                None => Value::Null,
            };
            (&mut request.intent_sha256, fields)
        }
        Desired::Codex(request) => (
            &mut request.intent_sha256,
            json!({"disabled":request.disabled}),
        ),
    };
    *hash = invocation::hash(&serde_json::to_vec(&(&*hash, client, fields))?);
    Ok(())
}
fn targets(
    mapping: &Mapping,
    reader: &mut Reader,
    root: &Directory,
    owner: &Owner,
    desired: Desired,
    work: &mut BTreeMap<PathBuf, Work>,
) -> Result<()> {
    let file = match owner.profile {
        ConfigProfile::Claude(_) => "settings.json",
        ConfigProfile::Codex => "config.toml",
    };
    let live = root.path.join(file);
    let binding = store::binding(root, &root.path)?;
    let mut live_owner = owner.clone();
    live_owner.role = Role::Live;
    let convert = |mut desired: Desired, role| {
        match &mut desired {
            Desired::Claude(r) => r.owner.role = role,
            Desired::Codex(r) => r.owner.role = role,
        }
        desired
    };
    add_target(
        work,
        live.clone(),
        Client {
            owner: live_owner,
            live_root: binding.clone(),
            live: live.clone(),
            source: None,
        },
        convert(desired.clone(), Role::Live),
    )?;
    if let Some(source) = mapping.entries.get(&live) {
        let roots = mapping.root.as_ref().ok_or_else(invalid)?;
        let source_root = reader
            .anchor(&roots.effective_source)?
            .ok_or_else(invalid)?;
        let relative = source
            .strip_prefix(&roots.effective_source)
            .map_err(|_| invalid())?
            .into();
        let mut source_owner = owner.clone();
        source_owner.role = Role::Source;
        let proof = StaticSource {
            root: store::binding(&source_root, &roots.effective_source)?,
            destination: roots.destination.clone(),
            relative,
        };
        if profile_authority::source_target(&proof)? != (source.clone(), live.clone()) {
            return Err(invalid());
        }
        add_target(
            work,
            source.clone(),
            Client {
                owner: source_owner,
                live_root: binding,
                live,
                source: Some(proof),
            },
            convert(desired, Role::Source),
        )?;
    } else if !mapping.unmanaged {
        return Err(invalid());
    }
    Ok(())
}

fn pstack_request(
    reader: &mut Reader,
    root: &Directory,
    owner: &Owner,
    allowed: Option<&BTreeSet<String>>,
    hash: &str,
) -> Result<claude_fields::Request> {
    let (desired, intent_sha256) = if let Some(allowed) = allowed {
        let (entries, catalog) = pstack_catalog(reader, &root.path, allowed)?;
        (
            Some(claude_fields::Desired::Pstack(entries)),
            invocation::hash(&serde_json::to_vec(&(hash, catalog))?),
        )
    } else {
        (None, hash.into())
    };
    Ok(claude_fields::Request {
        owner: owner.clone(),
        intent_sha256,
        desired,
    })
}
fn owners(file: &profile_registry::File) -> Vec<&Owner> {
    match &file.fields {
        profile_registry::Fields::Claude { plugins, pstack } => plugins
            .values()
            .flat_map(|f| f.claims.values().map(|c| &c.owner))
            .chain(
                pstack
                    .iter()
                    .flat_map(|r| r.claims.values().map(|c| &c.owner)),
            )
            .collect(),
        profile_registry::Fields::Codex { skills } => skills
            .values()
            .flat_map(|f| f.enabled.claims.values().map(|c| &c.owner))
            .collect(),
    }
}
fn had_intent(book: &invocation::OverlayRecord, profile: ConfigProfile, intent: Intent) -> bool {
    book.profiles
        .values()
        .flat_map(owners)
        .any(|o| o.profile == profile && o.intent == intent)
}
fn disabled_paths(
    ctx: &Context,
    reader: &mut Reader,
    names: &BTreeSet<String>,
) -> Result<BTreeSet<PathBuf>> {
    let mut paths = BTreeSet::new();
    for name in names {
        let path = ctx.home.join(".agents/skills").join(name).join("SKILL.md");
        if reader.regular(&path)?.is_none() {
            return Err(invalid());
        }
        paths.insert(path);
    }
    Ok(paths)
}
struct IntentInput<'a> {
    rules: &'a Profiles,
    book: &'a invocation::OverlayRecord,
    selected: &'a Selected,
    hash: &'a str,
}
fn profile_work(
    ctx: &Context,
    reader: &mut Reader,
    input: &IntentInput<'_>,
    map: &Mapping,
    work: &mut BTreeMap<PathBuf, Work>,
) -> Result<Option<PathBuf>> {
    let IntentInput {
        rules,
        book,
        selected,
        hash,
    } = input;
    let mut codex = None;
    for profile in &selected.profiles {
        match profile {
            ConfigProfile::Claude(profile) => {
                let rule = &rules.rules[profile];
                let plugins = rule.plugins_off.is_some()
                    || had_intent(book, ConfigProfile::Claude(*profile), Intent::Plugins);
                let pstack = rule.pstack_allow.is_some()
                    || had_intent(book, ConfigProfile::Claude(*profile), Intent::Pstack);
                if !plugins && !pstack {
                    continue;
                }
                let root = reader
                    .anchor(&profile.root(&ctx.home)?)?
                    .ok_or_else(invalid)?;
                if plugins {
                    let owner = Owner {
                        profile: ConfigProfile::Claude(*profile),
                        role: Role::Live,
                        intent: Intent::Plugins,
                    };
                    let request = claude_fields::Request {
                        owner: owner.clone(),
                        intent_sha256: (*hash).into(),
                        desired: rule
                            .plugins_off
                            .clone()
                            .map(claude_fields::Desired::Plugins),
                    };
                    targets(map, reader, &root, &owner, Desired::Claude(request), work)?;
                }
                if pstack {
                    let owner = Owner {
                        profile: ConfigProfile::Claude(*profile),
                        role: Role::Live,
                        intent: Intent::Pstack,
                    };
                    let request =
                        pstack_request(reader, &root, &owner, rule.pstack_allow.as_ref(), hash)?;
                    targets(map, reader, &root, &owner, Desired::Claude(request), work)?;
                }
            }
            ConfigProfile::Codex => {
                if rules.codex_disabled.is_none()
                    && !had_intent(book, ConfigProfile::Codex, Intent::CodexSkills)
                {
                    continue;
                }
                let path = ctx
                    .harness
                    .codex
                    .clone()
                    .unwrap_or_else(|| ctx.home.join(".codex"));
                let root = reader.anchor(&path)?.ok_or_else(invalid)?;
                codex = Some(root.path.clone());
                let disabled = rules
                    .codex_disabled
                    .as_ref()
                    .map(|names| disabled_paths(ctx, reader, names))
                    .transpose()?;
                let owner = Owner {
                    profile: ConfigProfile::Codex,
                    role: Role::Live,
                    intent: Intent::CodexSkills,
                };
                let request = codex_profile::Request {
                    owner: owner.clone(),
                    intent_sha256: (*hash).into(),
                    disabled,
                };
                targets(map, reader, &root, &owner, Desired::Codex(request), work)?;
            }
        }
    }
    Ok(codex)
}
fn pending_policies(
    rules: &Profiles,
    book: &invocation::OverlayRecord,
    selected: &Selected,
) -> Vec<Value> {
    let mut labels: BTreeSet<_> = rules.pending_config_policies().into_iter().collect();
    for owner in book.profiles.values().flat_map(owners) {
        let label = match (owner.profile, owner.intent) {
            (ConfigProfile::Claude(p), Intent::Plugins) => {
                format!("claude:{}:plugins_off", p.name())
            }
            (ConfigProfile::Claude(p), Intent::Pstack) => {
                format!("claude:{}:pstack_allow", p.name())
            }
            (ConfigProfile::Codex, Intent::CodexSkills) => "codex:disable_agents_skills".into(),
            _ => continue,
        };
        labels.insert(label);
    }
    labels
        .into_iter()
        .filter(|label| {
            if label.starts_with("codex:") {
                !selected.profiles.contains(&ConfigProfile::Codex)
            } else {
                !Profile::ALL.into_iter().any(|p| {
                    selected.profiles.contains(&ConfigProfile::Claude(p))
                        && label.starts_with(&format!("claude:{}:", p.name()))
                })
            }
        })
        .map(|intent| json!({"intent":intent,"reason":"not_selected"}))
        .collect()
}
fn relevant(rules: &Profiles, book: &invocation::OverlayRecord, selected: &Selected) -> bool {
    selected.profiles.iter().any(|profile| match profile {
        ConfigProfile::Claude(p) => {
            rules.rules[p].plugins_off.is_some()
                || rules.rules[p].pstack_allow.is_some()
                || had_intent(book, *profile, Intent::Plugins)
                || had_intent(book, *profile, Intent::Pstack)
        }
        ConfigProfile::Codex => {
            rules.codex_disabled.is_some() || had_intent(book, *profile, Intent::CodexSkills)
        }
    })
}
fn project_work(
    reader: &mut Reader,
    book: &mut invocation::OverlayRecord,
    work: BTreeMap<PathBuf, Work>,
) -> Result<(Vec<Patch>, BTreeMap<PathBuf, Target>)> {
    let mut patches = vec![];
    let mut targets_map = BTreeMap::new();
    for (path, row) in work {
        crate::platform::fs::write_path(&path)?;
        let before_bytes = reader.regular(&path)?.ok_or_else(|| {
            Error::attention(
                "profile_config_file_missing",
                "配置文件缺失；不复活用户删除的配置",
            )
        })?;
        let namespace = store::namespace(&path)?;
        let (before, verified) = store::observe(&namespace, 1024 * 1024)?;
        if verified.as_deref() != Some(before_bytes.as_slice()) {
            return Err(invalid());
        }
        let (after, record) = if row.desired.iter().all(|r| matches!(r, Desired::Claude(_))) {
            let requests: Vec<_> = row
                .desired
                .iter()
                .filter_map(|r| {
                    if let Desired::Claude(r) = r {
                        Some(r.clone())
                    } else {
                        None
                    }
                })
                .collect();
            let proposal =
                claude_fields::plan(&path, &before_bytes, book.profiles.get(&path), &requests)?;
            (proposal.after, proposal.record)
        } else if row.desired.iter().all(|r| matches!(r, Desired::Codex(_))) {
            let requests: Vec<_> = row
                .desired
                .iter()
                .filter_map(|r| {
                    if let Desired::Codex(r) = r {
                        Some(r.clone())
                    } else {
                        None
                    }
                })
                .collect();
            let proposal =
                codex_profile::plan(&path, &before_bytes, book.profiles.get(&path), &requests)?;
            (proposal.after, proposal.record)
        } else {
            return Err(invalid());
        };
        if let Some(record) = record {
            book.profiles.insert(path.clone(), record);
        } else {
            book.profiles.remove(&path);
        }
        if after != before_bytes {
            let crate::capability::rules::capability_tx::State::File { mode, .. } = before else {
                return Err(invalid());
            };
            patches.push(Patch {
                path: path.clone(),
                before,
                namespace,
                before_bytes,
                after,
                mode,
            });
        }
        targets_map.insert(
            path,
            Target {
                clients: row.clients,
            },
        );
    }
    Ok((patches, targets_map))
}
pub fn assess(ctx: &Context, authority: &Governance, selected: &Selected) -> Result<Assessment> {
    authority.ensure_settled()?;
    let mut reader = Reader::new();
    reader.regular(&ctx.state.join("migration-ledger.json"))?;
    let raw = dataset(&mut reader, authority, Dataset::SkillProfiles)?;
    let rules = raw
        .as_deref()
        .map(delivery::profiles)
        .transpose()?
        .unwrap_or_default();
    let hash = invocation::hash(raw.as_deref().unwrap_or(b"{}"));
    let book_before = dataset(&mut reader, authority, Dataset::InvocationOverlays)?;
    let mut book = book_before
        .as_deref()
        .map(|b| invocation::record(&ctx.home, b))
        .transpose()?
        .unwrap_or(invocation::OverlayRecord {
            schema_version: 1,
            policy_hash: None,
            files: BTreeMap::new(),
            profile_policy_hash: None,
            profiles: BTreeMap::new(),
        });
    let pending = pending_policies(&rules, &book, selected);
    let relevant = relevant(&rules, &book, selected);
    if !relevant {
        return Ok(Assessment {
            reader,
            authority: Authority {
                mapping_inputs: ctx.chezmoi.clone(),
                isolated: ctx.isolated,
                codex_root: None,
                targets: BTreeMap::new(),
            },
            patches: vec![],
            book_before: book_before.clone(),
            book_after: book_before,
            pending,
        });
    }
    let map = mapping(ctx, &mut reader)?;
    let mut work = BTreeMap::new();
    let codex_root = profile_work(
        ctx,
        &mut reader,
        &IntentInput {
            rules: &rules,
            book: &book,
            selected,
            hash: &hash,
        },
        &map,
        &mut work,
    )?;
    for old in book.profiles.values() {
        if owners(old)
            .iter()
            .any(|o| selected.profiles.contains(&o.profile))
            && !work.contains_key(&old.target)
        {
            return Err(Error::attention(
                "profile_source_mapping_changed",
                "旧配置 owner 的来源映射已变化，未猜测退役其他文件",
            ));
        }
    }
    let (patches, targets_map) = project_work(&mut reader, &mut book, work)?;
    book.schema_version = 2;
    book.profile_policy_hash = if pending.is_empty() && selected.profiles.len() == 5 {
        Some(hash)
    } else {
        None
    };
    let mut encoded = serde_json::to_vec_pretty(&book)?;
    encoded.push(b'\n');
    let book_after = if book_before.as_ref() == Some(&encoded) {
        book_before.clone()
    } else {
        Some(encoded)
    };
    let authority = Authority {
        mapping_inputs: ctx.chezmoi.clone(),
        isolated: ctx.isolated,
        codex_root,
        targets: targets_map,
    };
    authority.validate(&ctx.home)?;
    reader.verify()?;
    Ok(Assessment {
        reader,
        authority,
        patches,
        book_before,
        book_after,
        pending,
    })
}

#[cfg(test)]
#[path = "../../tests/unit/profile_inventory.rs"]
mod tests;
