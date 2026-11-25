use crate::capability::scoped_files;
use crate::{
    context::Context,
    error::{Error, Result},
    platform::fs::Directory,
    policy::{
        native_skills::Recipe,
        source_git::{ObjectId, RelativePath, Repository},
        source_payload::Rules,
    },
    skills::native_skills_files as files,
    source::source_git::Git,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::PathBuf};

fn invalid() -> Error {
    Error::attention(
        "native_skills_artifact_unverified",
        "自有派生产物、许可证或上游目录身份无法验证",
    )
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepared {
    pub local: Option<PathBuf>,
    pub local_digest: Option<String>,
    pub licenses: BTreeMap<String, Vec<u8>>,
    pub upstream_tree: String,
}
fn repository(ctx: &Context, recipe: &Recipe) -> Result<(tempfile::TempDir, Git, Repository)> {
    let scratch = scoped_files::private(&ctx.cache.join("native-skills/source"))?;
    let directory = tempfile::Builder::new()
        .prefix("macops-native-source-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&scratch.path)?;
    let repository = Repository::parse(&recipe.source.repository)?;
    let mut git = Git::create_bound(&directory.path().join("git"), &repository)?;
    git.set_storage_budget(256 * 1024 * 1024)?;
    Ok((directory, git, repository))
}
fn license_bytes(git: &Git, tree: &ObjectId, recipe: &Recipe) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut licenses = BTreeMap::new();
    for path in &recipe.license_files {
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|p| p.to_str())
            .ok_or_else(invalid)?;
        let bytes =
            crate::source::source_payload::read_file(git, tree, &RelativePath::parse(path)?)?;
        if licenses.insert(name.into(), bytes).is_some() {
            return Err(invalid());
        }
    }
    Ok(licenses)
}

pub fn prepare(ctx: &Context, recipe: &Recipe) -> Result<Prepared> {
    if recipe.license_files.is_empty() && recipe.derived.is_none() {
        return Ok(Prepared {
            local: None,
            local_digest: None,
            licenses: BTreeMap::new(),
            upstream_tree: String::new(),
        });
    }
    let (_directory, mut git, repository) = repository(ctx, recipe)?;
    let tree = git.fetch_commit(&repository, &ObjectId::parse(&recipe.source.commit)?)?;
    prepare_fetched(ctx, recipe, &git, &tree)
}

fn prepare_fetched(ctx: &Context, recipe: &Recipe, git: &Git, tree: &ObjectId) -> Result<Prepared> {
    let subtree = crate::source::source_payload::directory_object(
        git,
        tree,
        &RelativePath::parse(&recipe.source.skill_directory)?,
    )?;
    let licenses = license_bytes(git, tree, recipe)?;
    let Some(value) = &recipe.derived else {
        return Ok(Prepared {
            local: None,
            local_digest: None,
            licenses,
            upstream_tree: subtree.as_str().into(),
        });
    };
    let rules = Rules::declared(value)?;
    let prepared = crate::source::source_payload::read(git, tree, &rules)?;
    if prepared.repository != Repository::parse(&recipe.source.repository)?
        || prepared.source_path != RelativePath::parse(&recipe.source.skill_directory)?
        || prepared.skill_tree != subtree
    {
        return Err(invalid());
    }
    derived_artifact(ctx, recipe, &prepared, &subtree)
}
fn derived_artifact(
    ctx: &Context,
    recipe: &Recipe,
    prepared: &crate::source::source_payload::Prepared,
    subtree: &ObjectId,
) -> Result<Prepared> {
    let value = recipe.derived.as_ref().ok_or_else(invalid)?;
    let shape = prepared.payload.shape(0, 0o700);
    let manifest =
        serde_json::json!({"schemaVersion":1,"upstream":recipe.source,"rules":value,"shape":shape});
    let hash = crate::domain::fingerprint(&manifest)?;
    let root = scoped_files::private(&ctx.data.join("native-skills/artifacts"))?;
    let artifact = root.path.join(&hash);
    let payload = artifact.join("payload");
    if artifact.exists() {
        let artifact_root = scoped_files::private(&artifact)?;
        if crate::capability::capability_guard::listing(&artifact_root)?
            .into_keys()
            .collect::<std::collections::BTreeSet<_>>()
            != std::collections::BTreeSet::from(["manifest.json".into(), "payload".into()])
        {
            return Err(invalid());
        }
        if scoped_files::regular(&artifact.join("manifest.json"))?.as_deref()
            != Some(files::encode(&manifest)?.as_slice())
        {
            return Err(invalid());
        }
        let parent = Directory::open(&artifact)?;
        let slot = crate::policy::source_tree::Slot {
            parent: crate::capability::capability_store::binding(&parent, &artifact)?,
            name: "payload".into(),
        };
        let current = crate::source::source_tree::observe(&slot)?.ok_or_else(invalid)?;
        if current.shape()
            != prepared
                .payload
                .shape(nix::unistd::geteuid().as_raw(), 0o700)
        {
            return Err(invalid());
        }
    } else {
        let stage = tempfile::Builder::new()
            .prefix(".artifact-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(&root.path)?;
        let parent = Directory::open(stage.path())?;
        crate::source::source_payload::materialize(&parent, "payload", &prepared.payload, 0o700)?;
        crate::operations::store::Store::atomic_new(parent.file(), "manifest.json", &manifest)?;
        let staged = stage.keep();
        rustix::fs::renameat_with(
            root.file(),
            staged.file_name().ok_or_else(invalid)?,
            root.file(),
            hash.as_str(),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(std::io::Error::from)?;
        root.file().sync_all()?;
        root.verify()?;
    }
    let digest = artifact_digest(&payload)?;
    Ok(Prepared {
        local: Some(payload),
        local_digest: Some(digest),
        licenses: BTreeMap::new(),
        upstream_tree: subtree.as_str().into(),
    })
}

pub fn artifact_digest(path: &std::path::Path) -> Result<String> {
    let mut evidence = crate::agent::ai_payload::Evidence::default();
    evidence.tree(path.parent().ok_or_else(invalid)?)?;
    evidence.verify()?;
    evidence.digest()
}
impl Prepared {
    pub fn verify(&self) -> Result<()> {
        match (&self.local, &self.local_digest) {
            (Some(path), Some(digest)) if artifact_digest(path)? == *digest => Ok(()),
            (None, None) => Ok(()),
            _ => Err(invalid()),
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArtifactManifest {
    schema_version: u32,
    #[serde(deserialize_with = "crate::policy::json::object")]
    upstream: crate::policy::native_skills::Source,
    rules: serde_json::Value,
    #[serde(deserialize_with = "crate::policy::json::object")]
    shape: crate::policy::source_tree::Shape,
}
pub fn owned_local(
    ctx: &Context,
    recipe: &Recipe,
    path: &std::path::Path,
    exact_commit: bool,
) -> Result<()> {
    let artifact = path.parent().ok_or_else(invalid)?;
    if path.file_name() != Some(std::ffi::OsStr::new("payload"))
        || artifact.parent() != Some(ctx.data.join("native-skills/artifacts").as_path())
    {
        return Err(invalid());
    }
    let raw = scoped_files::regular(&artifact.join("manifest.json"))?.ok_or_else(invalid)?;
    let value: serde_json::Value = crate::policy::native_skills::decode(&raw)?;
    if artifact.file_name().and_then(|p| p.to_str())
        != Some(crate::domain::fingerprint(&value)?.as_str())
    {
        return Err(invalid());
    }
    let mut manifest: ArtifactManifest = crate::policy::native_skills::decode(&raw)?;
    if !exact_commit {
        manifest.upstream.commit.clone_from(&recipe.source.commit);
    }
    if manifest.schema_version != 1
        || manifest.upstream != recipe.source
        || Some(&manifest.rules) != recipe.derived.as_ref()
    {
        return Err(invalid());
    }
    manifest.shape.validate(0)?;
    let files::Object::Directory { tree, .. } = files::capture(path)? else {
        return Err(invalid());
    };
    let mut shape = tree.shape();
    shape.uid = 0;
    for entry in shape.entries.values_mut() {
        entry.uid = 0;
    }
    if shape != manifest.shape {
        return Err(invalid());
    }
    Ok(())
}
pub fn installed_derived(ctx: &Context, recipe: &Recipe, path: &std::path::Path) -> Result<()> {
    owned_local(ctx, recipe, path, true)?;
    let files::Object::Directory { tree: original, .. } = files::capture(path)? else {
        return Err(invalid());
    };
    let files::Object::Directory {
        tree: installed, ..
    } = files::capture(&files::payload(ctx, &recipe.install_name)?)?
    else {
        return Err(invalid());
    };
    let content = |tree: crate::policy::source_tree::Tree| {
        tree.entries
            .into_iter()
            .map(|(name, e)| (name, e.content))
            .collect::<BTreeMap<_, _>>()
    };
    if content(original) != content(installed) {
        return Err(invalid());
    }
    Ok(())
}
pub fn candidate(ctx: &Context, recipe: &Recipe) -> Result<(String, String, String)> {
    let (_directory, mut git, repository) = repository(ctx, recipe)?;
    let reference =
        crate::policy::source_git::Reference::parse(recipe.source.tracking_ref.as_deref())?;
    let commit = if recipe.source.tracking_ref.is_some() {
        git.resolve(&repository, &reference)?
    } else {
        ObjectId::parse(&recipe.source.commit)?
    };
    let tree = git.fetch_commit(&repository, &commit)?;
    let (identity, subtree) = candidate_identity(&git, &tree, recipe)?;
    Ok((commit.as_str().into(), identity, subtree))
}

fn candidate_identity(git: &Git, tree: &ObjectId, recipe: &Recipe) -> Result<(String, String)> {
    let subtree = crate::source::source_payload::directory_object(
        git,
        tree,
        &RelativePath::parse(&recipe.source.skill_directory)?,
    )?;
    let mut license_hashes = vec![];
    for path in &recipe.license_files {
        license_hashes.push((
            path,
            crate::capability::capability_store::hash(&crate::source::source_payload::read_file(
                git,
                tree,
                &RelativePath::parse(path)?,
            )?),
        ));
    }
    let identity = if let Some(value) = &recipe.derived {
        let prepared = crate::source::source_payload::read(git, tree, &Rules::declared(value)?)?;
        crate::domain::fingerprint(&prepared.payload.shape(0, 0o700))?
    } else {
        crate::domain::fingerprint(&(subtree.as_str(), license_hashes))?
    };
    Ok((identity, subtree.as_str().into()))
}

#[cfg(test)]
#[path = "../../tests/unit/native_skills_assets.rs"]
mod tests;
