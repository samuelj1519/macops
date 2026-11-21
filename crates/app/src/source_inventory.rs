pub use crate::policy::source_tx::NativeSpec;
use crate::{
    capability_local, capability_store,
    context::Context,
    domain::{CapabilityKind, Provider},
    error::{Error, Result},
    governance::Governance,
    link_inventory::Reader,
    paths::Directory,
    policy::{
        governance::Dataset,
        source_baseline::{self, Baseline, Identity},
        source_git::{RelativePath, Repository},
        source_tree::{Slot, Tree},
    },
    source_tree,
};
use serde_json::Value;
pub struct Adoption {
    pub reader: Reader,
    pub baseline: Baseline,
    pub tree: Tree,
    pub managed_before: Option<Vec<u8>>,
    pub managed_after: Vec<u8>,
    pub native: Option<NativeSpec>,
}
fn invalid() -> Error {
    Error::attention(
        "unverified_source_installation",
        "来源登记、目录或原生记录无法验证",
    )
}
fn dataset(reader: &mut Reader, authority: &Governance, key: Dataset) -> Result<Option<Vec<u8>>> {
    let bytes = authority.read(key, 16 * 1024 * 1024)?;
    if reader.regular(&authority.paths.dataset(key))? != bytes {
        return Err(invalid());
    }
    Ok(bytes)
}
fn root(ctx: &Context, name: &str) -> Result<Slot> {
    crate::policy::source_git::Component::parse(name)?;
    let path = ctx.home.join(".agents/skills");
    let directory = Directory::open(&path)?;
    Ok(Slot {
        parent: capability_store::binding(&directory, &path)?,
        name: name.into(),
    })
}
fn alias_collision(
    ctx: &Context,
    reader: &mut Reader,
    authority: &Governance,
    key: &str,
) -> Result<bool> {
    let name = if let Some(name) = key.strip_prefix("skill:") {
        name
    } else if key.contains(':') {
        return Ok(false);
    } else {
        key
    };
    let definitions = dataset(reader, authority, Dataset::AssetDefinitions)?;
    let declared = definitions
        .as_deref()
        .map(crate::policy::json_fields::parse_governance)
        .transpose()?;
    let assets = declared
        .as_ref()
        .map(|v| {
            v.get("assets")
                .and_then(Value::as_object)
                .ok_or_else(invalid)
        })
        .transpose()?;
    let catalog = reader.regular(&ctx.catalog)?;
    if catalog.is_none() && ctx.catalog_explicit {
        return Err(invalid());
    }
    let custom = if let Some(bytes) = catalog {
        let value = crate::policy::json::parse(&bytes)?;
        let crate::policy::json::Object(catalog) =
            serde_json::from_value::<crate::policy::json::Object<crate::domain::Catalog>>(value)?;
        catalog.validate()?;
        catalog.packages.iter().any(|p| {
            (p.id == key || p.id == format!("skill:{name}") || p.name == name)
                && matches!(&p.provider,Provider::Legacy{category,..} if category == "skills")
        })
    } else {
        false
    };
    Ok(custom || assets.is_some_and(|rows| rows.contains_key(name)))
}
fn native_identity(
    ctx: &Context,
    reader: &mut Reader,
    authority: &Governance,
    key: &str,
) -> Result<Option<(Identity, NativeSpec)>> {
    if key.starts_with("plugin:") {
        return Ok(None);
    }
    if !key.starts_with("skill:") && !key.starts_with("capability:") {
        if alias_collision(ctx, reader, authority, key)? {
            return Err(Error::attention(
                "ambiguous",
                "同名来源有多个登记；请使用明确 capability: 或 asset: ID",
            ));
        }
        return Ok(None);
    }
    let record = crate::policy::delivery::native_lock_path(&ctx.home, &ctx.scope);
    let Some(bytes) = reader.regular(&record)? else {
        return Ok(None);
    };
    let document = match crate::policy::json::parse(&bytes) {
        Ok(document) => document,
        Err(error) if key.starts_with("skill:") => return Err(error),
        Err(_) => return Ok(None),
    };
    let Some(rows) = document.get("skills").and_then(Value::as_object) else {
        return if key.starts_with("skill:") {
            Err(invalid())
        } else {
            Ok(None)
        };
    };
    let mut selected = None;
    for (name, value) in rows {
        let source = value.get("source").and_then(Value::as_str);
        let kind = value.get("sourceType").and_then(Value::as_str);
        let exact = source
            .zip(kind)
            .map(|(source, kind)| {
                crate::policy::capability_local::skill_capability_id(&record, name, kind, source)
            })
            .transpose()?;
        let logical = crate::policy::native_skills::installation_id(&crate::paths::read_path(
            &ctx.home.join(".agents/skills").join(name),
        )?)?;
        if key == format!("skill:{name}") || exact.as_deref() == Some(key) || key == logical {
            if kind != Some("github") {
                return Ok(None);
            }
            if document.get("version").and_then(Value::as_u64) != Some(3) {
                return Err(invalid());
            }
            if alias_collision(ctx, reader, authority, key)? {
                return Err(Error::attention(
                    "ambiguous",
                    "同名来源有多个登记；请使用明确 capability: 或 asset: ID",
                ));
            }
            selected = Some((
                name,
                value.as_object().ok_or_else(invalid)?,
                source.ok_or_else(invalid)?,
            ));
            break;
        }
    }
    let Some((native_id, row, source)) = selected else {
        return Ok(None);
    };
    crate::policy::source_git::Component::parse(native_id)?;
    let payload = ctx.home.join(".agents/skills").join(native_id);
    let package = github_package(&record, native_id, source, payload)?;
    let raw_path = row
        .get("skillPath")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let path = crate::policy::native_skills::skill_directory(raw_path)?;
    let repository = Repository::parse(source)?;
    repository.verify_url(
        row.get("sourceUrl")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?,
    )?;
    let query = capability_local::capture(&package)?;
    let expected = capability_local::observe_default(&package.id, &query)?;
    let source = Identity {
        member: package.id.clone(),
        repository: repository.key().into(),
        skill_path: path,
        canonical_name: native_id.clone(),
    };
    Ok(Some((
        source,
        NativeSpec {
            package,
            query,
            expected,
        },
    )))
}
fn declared_identity(
    reader: &mut Reader,
    authority: &Governance,
    key: &str,
) -> Result<Option<Identity>> {
    let Some(name) = key.strip_prefix("asset:") else {
        return Ok(None);
    };
    crate::policy::source_git::Component::parse(name)?;
    let bytes = dataset(reader, authority, Dataset::AssetDefinitions)?.ok_or_else(invalid)?;
    let document = crate::policy::json::parse(&bytes)?;
    let row = document
        .get("assets")
        .and_then(Value::as_object)
        .and_then(|rows| rows.get(name))
        .and_then(Value::as_object)
        .ok_or_else(invalid)?;
    if row.get("kind").and_then(Value::as_str) != Some("github-skill") {
        return Err(Error::attention(
            "unsupported_source_kind",
            "该声明来源尚不属于 GitHub 完整根采纳范围",
        ));
    }
    let repository = Repository::parse(
        row.get("repository")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?,
    )?;
    let path = row
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    RelativePath::parse(path)?;
    Ok(Some(Identity {
        member: key.into(),
        repository: repository.key().into(),
        skill_path: path.into(),
        canonical_name: name.into(),
    }))
}
pub fn assess_adoption(
    ctx: &Context,
    authority: &Governance,
    key: &str,
) -> Result<Option<Adoption>> {
    authority.ensure_settled()?;
    let mut reader = Reader::new();
    let (identity, native) = if let Some(identity) = declared_identity(&mut reader, authority, key)?
    {
        let lock = crate::policy::delivery::native_lock_path(&ctx.home, &ctx.scope);
        if let Some(bytes) = reader.regular(&lock)? {
            let document = crate::policy::json::parse(&bytes)?;
            if document.get("version").and_then(Value::as_u64) != Some(3)
                || !document.get("skills").is_some_and(Value::is_object)
            {
                return Err(Error::attention(
                    "unsupported_native_source_registry",
                    "原生技能登记版本或成员表未知，不能证明声明资产没有归属冲突",
                ));
            }
            if document
                .get("skills")
                .and_then(Value::as_object)
                .is_some_and(|rows| rows.contains_key(&identity.canonical_name))
            {
                return Err(Error::attention(
                    "ambiguous_source_registration",
                    "同一目录同时有原生和声明来源；请使用明确原生安装 ID",
                ));
            }
        }
        (identity, None)
    } else if let Some((identity, spec)) = native_identity(ctx, &mut reader, authority, key)? {
        (identity, Some(spec))
    } else {
        return Ok(None);
    };
    reader.regular(&ctx.state.join("migration-ledger.json"))?;
    let slot = root(ctx, &identity.canonical_name)?;
    source_baseline::disjoint(
        &slot.path(),
        &[
            ctx.settings.clone(),
            ctx.catalog.clone(),
            ctx.config.clone(),
            ctx.data.clone(),
            ctx.state.clone(),
            ctx.cache.clone(),
            authority.paths.backups.clone(),
            authority.paths.pending.clone(),
            authority.paths.managed.clone(),
            authority.paths.overlays.clone(),
            authority.paths.manifests.clone(),
            crate::policy::delivery::native_lock_path(&ctx.home, &ctx.scope),
        ],
    )?;
    let tree = source_tree::observe(&slot)?.ok_or_else(invalid)?;
    let baseline = Baseline::capture(
        identity,
        slot,
        &tree,
        &ctx.home,
        nix::unistd::geteuid().as_raw(),
    )?;
    let managed_before = dataset(&mut reader, authority, Dataset::ManagedAssets)?;
    let managed_after = source_baseline::adopt(
        managed_before.as_deref(),
        baseline.clone(),
        &ctx.home,
        nix::unistd::geteuid().as_raw(),
    )?;
    reader.verify()?;
    authority.verify()?;
    let result = Adoption {
        reader,
        baseline,
        tree,
        managed_before,
        managed_after,
        native,
    };
    result.verify()?;
    Ok(Some(result))
}
impl Adoption {
    pub fn verify(&self) -> Result<()> {
        self.reader.verify()?;
        if source_tree::observe(&self.baseline.slot)?.as_ref() != Some(&self.tree) {
            return Err(invalid());
        }
        if let Some(spec) = &self.native
            && capability_local::observe_default(&spec.package.id, &spec.query)? != spec.expected
        {
            return Err(invalid());
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/unit/source_inventory.rs"]
mod tests;

pub fn project_metadata(
    ctx: &Context,
    before: Option<&[u8]>,
    changes: &[crate::policy::capability_tx::FileChange],
) -> Result<Option<Vec<u8>>> {
    let owner = nix::unistd::geteuid().as_raw();
    let book = source_baseline::read(before, &ctx.home, owner)?;
    let mut output = before.map(<[u8]>::to_vec);
    for baseline in book
        .roots
        .values()
        .filter(|b| changes.iter().any(|c| c.path.starts_with(b.slot.path())))
    {
        let current = source_tree::observe(&baseline.slot)?.ok_or_else(invalid)?;
        baseline.authorize(&baseline.source, &baseline.slot, &current, &ctx.home, owner)?;
        let after = source_baseline::project(baseline, changes, &ctx.home, owner)?;
        output = Some(source_baseline::adopt(
            output.as_deref(),
            after,
            &ctx.home,
            owner,
        )?);
    }
    Ok(output)
}
pub fn verify_metadata(
    home: &std::path::Path,
    raw: Option<&[u8]>,
    changes: &[crate::policy::capability_tx::FileChange],
) -> Result<()> {
    let owner = nix::unistd::geteuid().as_raw();
    let book = source_baseline::read(raw, home, owner)?;
    for baseline in book
        .roots
        .values()
        .filter(|b| changes.iter().any(|c| c.path.starts_with(b.slot.path())))
    {
        let current = source_tree::observe(&baseline.slot)?.ok_or_else(invalid)?;
        baseline.authorize(&baseline.source, &baseline.slot, &current, home, owner)?;
    }
    Ok(())
}

fn github_package(
    record: &std::path::Path,
    native_id: &str,
    source: &str,
    payload: std::path::PathBuf,
) -> Result<crate::domain::Package> {
    let package = crate::domain::Package {
        id: crate::policy::capability_local::skill_capability_id(
            record, native_id, "github", source,
        )?,
        name: native_id.into(),
        kind: crate::domain::Kind::Skill,
        official_url: "https://github.com/vercel-labs/skills".into(),
        paths: vec![crate::domain::OwnedPath {
            path: payload.clone(),
            kind: crate::domain::PathKind::Payload,
        }],
        provider: Provider::CapabilityObserved {
            capability: CapabilityKind::SkillsCli,
            native_id: native_id.into(),
            manager: "skills-cli:github".into(),
            record: record.into(),
            payload,
            source: Some(source.into()),
            profile_id: None,
            settings: None,
        },
        exact_version_required: false,
    };
    Ok(package)
}
