use crate::{
    capability::{
        capability_guard, capability_store as store,
        rules::{
            capability_tx::{DirectoryWitness, LinkAuthority, Namespace, State, Witness},
            delivery::{
                self, LinkRule, LinkText, LinkUnit, Profile, Profiles, Selection, SourceGroup,
            },
        },
    },
    context::Context,
    error::{Error, Result},
    operations::governance::Governance,
    platform::fs::{Directory, EntryKind},
    policy::governance::Dataset,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::{Path, PathBuf},
};

fn invalid() -> Error {
    Error::attention(
        "invalid_link_inventory",
        "能力投放来源、作用域或目录无法验证",
    )
}
struct FileSnapshot {
    witness: Witness,
    bytes: Option<Vec<u8>>,
}
pub struct Reader {
    files: BTreeMap<PathBuf, FileSnapshot>,
    directories: BTreeMap<PathBuf, DirectoryWitness>,
    budget: u64,
    anchors: Vec<crate::policy::governance::DirectoryBinding>,
}
impl Reader {
    pub(crate) fn new() -> Self {
        Self {
            files: BTreeMap::new(),
            directories: BTreeMap::new(),
            budget: 64 * 1024 * 1024,
            anchors: vec![],
        }
    }
    fn file(&mut self, path: &Path) -> Result<&FileSnapshot> {
        if !self.files.contains_key(path) {
            if self.files.len() >= 10000 {
                return Err(invalid());
            }
            let namespace = store::namespace(path)?;
            let (state, bytes) = store::observe(&namespace, self.budget.min(16 * 1024 * 1024))?;
            self.budget = self
                .budget
                .saturating_sub(bytes.as_ref().map_or(0, |b| b.len() as u64));
            self.files.insert(
                path.into(),
                FileSnapshot {
                    witness: Witness {
                        path: path.into(),
                        namespace,
                        state,
                    },
                    bytes,
                },
            );
        }
        self.files.get(path).ok_or_else(invalid)
    }
    pub(crate) fn regular(&mut self, path: &Path) -> Result<Option<Vec<u8>>> {
        let file = self.file(path)?;
        if matches!(file.witness.state, State::Link { .. }) {
            return Err(invalid());
        }
        Ok(file.bytes.clone())
    }
    pub(crate) fn anchor(&mut self, path: &Path) -> Result<Option<Directory>> {
        crate::platform::fs::write_path(path)?;
        let root = match Directory::open(path) {
            Ok(root) => root,
            Err(error) if error.code == "path_missing" => {
                self.file(path)?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let binding = store::binding(&root, path)?;
        if binding.uid != nix::unistd::geteuid().as_raw() || binding.requested != binding.canonical
        {
            return Err(invalid());
        }
        self.anchors.push(binding);
        Ok(Some(root))
    }
    pub(crate) fn directory(&mut self, path: &Path) -> Result<Option<Directory>> {
        let Some(root) = self.anchor(path)? else {
            return Ok(None);
        };
        if self.directories.len() >= 512 {
            return Err(invalid());
        }
        let snapshot = DirectoryWitness {
            binding: store::binding(&root, path)?,
            entries: capability_guard::listing(&root)?,
        };
        if self
            .directories
            .get(path)
            .is_some_and(|old| old != &snapshot)
        {
            return Err(invalid());
        }
        self.directories.insert(path.into(), snapshot);
        Ok(Some(root))
    }
    pub fn verify(&self) -> Result<()> {
        for binding in &self.anchors {
            store::open(&Namespace {
                ancestor: binding.clone(),
                remaining: vec![".probe".into()],
            })?;
        }
        for file in self.files.values() {
            if store::observe(&file.witness.namespace, 16 * 1024 * 1024)?.0 != file.witness.state {
                return Err(invalid());
            }
        }
        for snapshot in self.directories.values() {
            let root = store::open(&Namespace {
                ancestor: snapshot.binding.clone(),
                remaining: vec![".probe".into()],
            })?;
            if capability_guard::listing(&root)? != snapshot.entries {
                return Err(invalid());
            }
        }
        Ok(())
    }
    pub fn witnesses(&self) -> (Vec<Witness>, Vec<DirectoryWitness>) {
        (
            self.files.values().map(|f| f.witness.clone()).collect(),
            self.directories.values().cloned().collect(),
        )
    }
}
pub struct Patch {
    pub path: PathBuf,
    pub namespace: Namespace,
    pub before: State,
    pub before_bytes: Option<Vec<u8>>,
    pub after: Option<LinkText>,
}
pub struct Assessment {
    pub reader: Reader,
    pub authority: LinkAuthority,
    pub patches: Vec<Patch>,
    pub rows: Vec<Value>,
    pub missing: Vec<Value>,
    pub conflicts: Vec<Value>,
}
impl Assessment {
    pub fn verify(&self) -> Result<()> {
        self.reader.verify()
    }
    pub fn summary(&self) -> Value {
        json!({"backend":"rust","scope":"global","operation":"links","profile_config_assessed":false,"status":if self.patches.is_empty(){"same"}else{"drift"},"incomplete":true,
            "pending_config_policies":self.authority.pending_config_policies,"missing":self.missing,"conflicts":self.conflicts,"results":self.rows,
            "planned_changes":self.patches.len(),"changes":self.patches.iter().map(|p|json!({"path":p.path,"action":if p.after.is_some(){"create"}else{"retire"}})).collect::<Vec<_>>(),"persistentWrites":false})
    }
}
fn dataset(reader: &mut Reader, authority: &Governance, key: Dataset) -> Result<Option<Vec<u8>>> {
    let bytes = authority.read(key, 16 * 1024 * 1024)?;
    if reader.regular(&authority.paths.dataset(key))? != bytes {
        return Err(invalid());
    }
    Ok(bytes)
}
fn canonical(
    ctx: &Context,
    reader: &mut Reader,
) -> Result<(
    crate::policy::governance::DirectoryBinding,
    BTreeMap<String, LinkUnit>,
)> {
    let path = ctx.home.join(".agents/skills");
    let root = reader.directory(&path)?.ok_or_else(|| {
        Error::attention(
            "canonical_skill_root_missing",
            "未找到全局 canonical 技能目录",
        )
    })?;
    let mut units = BTreeMap::new();
    for (name, entry) in capability_guard::listing(&root)? {
        if name.starts_with('.') {
            continue;
        }
        if entry.kind != "directory" {
            return Err(invalid());
        }
        LinkText::managed(&name)?;
        let child = root.child(OsStr::new(&name))?;
        let marker = child.path.join("SKILL.md");
        if reader.regular(&marker)?.is_none() {
            return Err(Error::attention(
                "canonical_skill_marker_missing",
                "canonical 子目录缺少可验证的 SKILL.md",
            ));
        }
        root.verify_child(OsStr::new(&name), &child)?;
        let id =
            crate::domain::resource::id("capability", &("untracked-skill", &path.join(&name)))?;
        units.insert(
            name.clone(),
            LinkUnit {
                id,
                name: name.clone(),
                aliases: BTreeSet::from([format!("skill:{name}")]),
            },
        );
    }
    root.verify()?;
    Ok((store::binding(&root, &path)?, units))
}
fn channels(bytes: Option<&[u8]>) -> Result<BTreeMap<String, String>> {
    let Some(bytes) = bytes else {
        return Ok(BTreeMap::new());
    };
    let value = crate::policy::json::parse(bytes)?;
    let root = value.as_object().ok_or_else(invalid)?;
    if root.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(invalid());
    }
    let Some(ids) = root.get("source_ids") else {
        return Ok(BTreeMap::new());
    };
    let map = ids.as_object().ok_or_else(invalid)?;
    map.iter()
        .map(|(source, id)| {
            let id = id
                .as_str()
                .filter(|s| crate::policy::catalog::resources::component(s) && s.len() <= 255)
                .ok_or_else(invalid)?;
            Ok((source.clone(), id.into()))
        })
        .collect()
}
fn source_groups(
    ctx: &Context,
    reader: &mut Reader,
    units: &mut BTreeMap<String, LinkUnit>,
    channel_bytes: Option<&[u8]>,
    facts: &delivery::AssetFacts,
) -> Result<Vec<SourceGroup>> {
    let lock = delivery::native_lock_path(&ctx.home, &ctx.scope);
    let aliases = channels(channel_bytes)?;
    let mut groups: BTreeMap<String, SourceGroup> = BTreeMap::new();
    let mut identities = BTreeMap::new();
    let mut observed = BTreeMap::new();
    if let Some(bytes) = reader.regular(&lock)? {
        let book = crate::policy::catalog::resources::skill_lock(&bytes, false)?;
        for (name, record) in book.skills {
            let Some(unit) = units.get_mut(&name) else {
                return Err(Error::attention(
                    "native_skill_payload_missing",
                    "原生登记包含缺失 canonical 载荷，保留链接待审",
                ));
            };
            unit.id = crate::capability::rules::capability_local::skill_capability_id(
                &lock,
                &name,
                &record.source_type,
                &record.source,
            )?;
            let id = aliases.get(&record.source).map_or_else(
                || crate::domain::resource::id("source", &(&record.source_type, &record.source)),
                |id| Ok(format!("source:{id}")),
            )?;
            let identity = (
                record.source_type.clone(),
                if record.source_type == "github" {
                    delivery::github_repository(&record.source)?
                } else {
                    record.source.clone()
                },
            );
            if identities
                .get(&id)
                .is_some_and(|existing| existing != &identity)
            {
                return Err(Error::attention(
                    "ambiguous_link_source_alias",
                    "同一来源 ID 指向多个不同原生来源，未建立投放批次",
                ));
            }
            identities.insert(id.clone(), identity.clone());
            observed.insert(name.clone(), identity);
            let group = groups.entry(id.clone()).or_insert_with(|| SourceGroup {
                id,
                aliases: BTreeSet::new(),
                members: BTreeSet::new(),
            });
            if let Some(alias) = aliases.get(&record.source)
                && alias != &name
            {
                group.aliases.insert(alias.clone());
            }
            group.members.insert(unit.id.clone());
        }
    }
    for name in &facts.standalone {
        let id = format!("source:{name}");
        let mut members = BTreeSet::new();
        if let Some((kind, repository)) = observed.get(name) {
            if kind != "github" || Some(repository) != facts.repositories.get(name) {
                return Err(Error::attention(
                    "declared_link_source_mismatch",
                    "资产声明与原生来源身份不一致，未建立投放批次",
                ));
            }
            members.insert(units.get(name).ok_or_else(invalid)?.id.clone());
        }
        if let Some(existing) = groups.get(&id) {
            if existing.members != members {
                return Err(Error::attention(
                    "ambiguous_link_source_alias",
                    "来源与资产声明的成员不一致",
                ));
            }
        } else {
            groups.insert(
                id.clone(),
                SourceGroup {
                    id,
                    aliases: BTreeSet::new(),
                    members,
                },
            );
        }
    }
    Ok(groups.into_values().collect())
}

fn profile_plan(
    ctx: &Context,
    reader: &mut Reader,
    profile: Profile,
    rules: &Profiles,
    selected: &BTreeMap<String, String>,
    protected: &BTreeSet<String>,
    plan: &mut Assessment,
) -> Result<()> {
    let path = profile.root(&ctx.home)?;
    let Some(root) = reader.anchor(&path)? else {
        if rules.declared.contains(&profile) {
            plan.missing
                .push(json!({"profile":profile,"reason":"profile_root_missing"}));
        } else {
            plan.rows.push(json!({"profile":profile,"status":"absent"}));
        }
        return Ok(());
    };
    let binding = store::binding(&root, &path)?;
    plan.authority.profiles.insert(profile, binding);
    if !protected.is_empty() {
        plan.authority.protected.insert(profile, protected.clone());
    }
    let skills = path.join("skills");
    let directory = reader.directory(&skills)?;
    if plan.authority.requested == Selection::All
        && let Some(directory) = &directory
    {
        for (name, _) in capability_guard::listing(directory)? {
            if !selected.contains_key(&name)
                && !name.to_ascii_lowercase().starts_with("devnorm")
                && !protected.contains(&name)
            {
                plan.conflicts.push(json!({"profile":profile,"name":name,"reason":"unselected_or_unknown_source_preserved"}));
            }
        }
    }
    let rule = &rules.rules[&profile].links;
    for name in selected.keys() {
        if name.to_ascii_lowercase().starts_with("devnorm") || protected.contains(name) {
            plan.rows
                .push(json!({"profile":profile,"name":name,"status":"native_owned_preserved"}));
            continue;
        }
        let target = delivery::link_target(&ctx.home, profile, name)?;
        let desired = match rule {
            LinkRule::All => true,
            LinkRule::Selected(names) => names.contains(name),
        };
        if let Some(directory) = &directory
            && let Some(meta) = directory.metadata(OsStr::new(name))?
            && matches!(meta.kind, EntryKind::Directory | EntryKind::Other)
        {
            plan.conflicts
                .push(json!({"profile":profile,"name":name,"reason":"unmanaged_non_link"}));
            continue;
        }
        let snapshot = reader.file(&target)?;
        let before = snapshot.witness.state.clone();
        let before_bytes = snapshot.bytes.clone();
        let namespace = snapshot.witness.namespace.clone();
        match &before {
            State::Missing if !desired => plan
                .rows
                .push(json!({"profile":profile,"name":name,"status":"absent"})),
            State::Missing => {
                plan.patches.push(Patch {
                    path: target,
                    namespace,
                    before,
                    before_bytes,
                    after: Some(LinkText::managed(name)?),
                });
                plan.rows
                    .push(json!({"profile":profile,"name":name,"status":"create"}));
            }
            State::Link { text, .. } if text.owned_by(name) => {
                if desired {
                    plan.rows
                        .push(json!({"profile":profile,"name":name,"status":"same"}));
                } else {
                    plan.patches.push(Patch {
                        path: target,
                        namespace,
                        before,
                        before_bytes,
                        after: None,
                    });
                    plan.rows
                        .push(json!({"profile":profile,"name":name,"status":"retire"}));
                }
            }
            _ => plan
                .conflicts
                .push(json!({"profile":profile,"name":name,"reason":"foreign_file_or_link"})),
        }
    }
    Ok(())
}
pub fn assess(ctx: &Context, authority: &Governance, request: Selection) -> Result<Assessment> {
    authority.ensure_settled()?;
    let mut reader = Reader::new();
    let profile_bytes = dataset(&mut reader, authority, Dataset::SkillProfiles)?;
    let rules = profile_bytes
        .as_deref()
        .map(delivery::profiles)
        .transpose()?
        .unwrap_or_default();
    let definition_bytes = dataset(&mut reader, authority, Dataset::AssetDefinitions)?;
    let facts = definition_bytes
        .as_deref()
        .map(delivery::asset_facts)
        .transpose()?
        .unwrap_or_default();
    let channel_bytes = dataset(&mut reader, authority, Dataset::SourceChannels)?;
    reader.regular(&ctx.state.join("migration-ledger.json"))?;
    let (canonical, mut units) = canonical(ctx, &mut reader)?;
    let groups = source_groups(
        ctx,
        &mut reader,
        &mut units,
        channel_bytes.as_deref(),
        &facts,
    )?;
    let all_units: Vec<_> = units.values().cloned().collect();
    let selected = delivery::resolve(&request, &all_units, &groups)?;
    let selected: BTreeMap<_, _> = all_units
        .into_iter()
        .filter(|u| selected.contains(&u.id))
        .map(|u| (u.name, u.id))
        .collect();
    let all = request == Selection::All;
    let mut plan = Assessment {
        reader: Reader::new(),
        authority: LinkAuthority {
            requested: request,
            canonical,
            selected: selected.clone(),
            profiles: BTreeMap::new(),
            pending_config_policies: rules.pending_config_policies(),
            protected: BTreeMap::new(),
        },
        patches: vec![],
        rows: vec![],
        missing: vec![],
        conflicts: vec![],
    };
    if all {
        for (profile, rule) in &rules.rules {
            if let LinkRule::Selected(names) = &rule.links {
                for name in names {
                    if !units.contains_key(name) {
                        plan.missing.push(
                            json!({"profile":profile,"name":name,"reason":"canonical_missing"}),
                        );
                    }
                }
            }
        }
    }
    for profile in Profile::ALL {
        profile_plan(
            ctx,
            &mut reader,
            profile,
            &rules,
            &selected,
            facts.copies.get(&profile).unwrap_or(&BTreeSet::new()),
            &mut plan,
        )?;
    }
    reader.verify()?;
    authority.verify()?;
    plan.reader = reader;
    Ok(plan)
}

#[cfg(test)]
#[path = "../../tests/unit/link_inventory.rs"]
mod tests;
