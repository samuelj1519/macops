use crate::{
    error::{Error, Result},
    native_plugins::valid_name,
    policy::source_git::{ObjectId, Reference, RelativePath, Repository},
    source_git::Git,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const MAX_TREE_BYTES: usize = 15 * 1024 * 1024;
const MARKET: &str = ".agents/plugins/marketplace.json";
const PLUGIN: &str = ".codex-plugin/plugin.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Offer {
    pub repository: String,
    pub marketplace: String,
    pub plugin: String,
    pub declared_version: String,
    pub marketplace_commit: String,
    pub payload_commit: String,
    pub payload_ref: String,
    pub marketplace_sha256: String,
    pub plugin_manifest_sha256: String,
}

pub(crate) fn invalid() -> Error {
    Error::attention(
        "codex_install_source_unverified",
        "无法将 Codex 插件安装绑定到已核验提交",
    )
}

impl Offer {
    pub fn parse(value: &Value) -> Result<Self> {
        let offer: Self = serde_json::from_value(value.clone())?;
        offer.validate()?;
        Ok(offer)
    }

    pub fn validate(&self) -> Result<()> {
        Repository::parse(&self.repository)?;
        ObjectId::parse(&self.marketplace_commit)?;
        if self.marketplace_commit != self.payload_commit
            || !matches!(
                Reference::parse(Some(&self.payload_ref))?,
                Reference::Named(_)
            )
            || !valid_name(&self.payload_ref)
            || ![&self.marketplace, &self.plugin, &self.declared_version]
                .into_iter()
                .all(|value| valid_name(value))
            || ![&self.marketplace_sha256, &self.plugin_manifest_sha256]
                .into_iter()
                .all(|hash| crate::policy::config_local::valid_digest(hash))
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn url(&self) -> Result<String> {
        Ok(Repository::parse(&self.repository)?.url())
    }

    pub fn native_id(&self) -> String {
        format!("{}@{}", self.plugin, self.marketplace)
    }
}

pub(crate) type Files = Vec<(String, bool, String)>;
pub(crate) type VerifiedFile<'a> = (&'a str, bool, &'a [u8]);

pub(crate) fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn acquire(offer: &Offer, root: &Path) -> Result<(PathBuf, Files)> {
    acquire_using(offer, root, |git, repository, commit, branch| {
        git.fetch_full_mirror(repository, commit, branch)
            .map(|(tree, _)| tree)
    })
}

pub(crate) fn acquire_using(
    offer: &Offer,
    root: &Path,
    fetch: impl FnOnce(&Git, &Repository, &ObjectId, &str) -> Result<ObjectId>,
) -> Result<(PathBuf, Files)> {
    offer.validate()?;
    let git = Git::create(root)?;
    let tree = fetch(
        &git,
        &Repository::parse(&offer.repository)?,
        &ObjectId::parse(&offer.payload_commit)?,
        &offer.payload_ref,
    )?;
    Ok((
        root.join("objects.git"),
        verified_mirror_contents(&git, &tree, offer)?,
    ))
}

pub(crate) fn verified_mirror_contents(git: &Git, tree: &ObjectId, offer: &Offer) -> Result<Files> {
    with_verified_files(git, tree, |contents| verify_codex_contents(contents, offer))
}

pub(crate) fn with_verified_files<T>(
    git: &Git,
    tree: &ObjectId,
    use_files: impl FnOnce(&[VerifiedFile<'_>]) -> Result<T>,
) -> Result<T> {
    let listing = parse_listing(&git.mirror_listing(tree)?)?;
    let objects = listing.iter().map(|file| &file.object).collect::<Vec<_>>();
    let bytes = git.mirror_blobs(&objects, 16 * 1024 * 1024)?;
    let contents = parse_batch(&listing, &bytes)?
        .into_iter()
        .map(|(file, bytes)| (file.path.as_str(), file.executable, bytes))
        .collect::<Vec<_>>();
    use_files(&contents)
}

fn verify_codex_contents(contents: &[VerifiedFile<'_>], offer: &Offer) -> Result<Files> {
    let select = |path: &str| {
        contents
            .iter()
            .find(|(name, _, _)| *name == path)
            .map(|(_, _, data)| *data)
    };
    let market = select(MARKET).ok_or_else(invalid)?;
    let plugin = select(PLUGIN).ok_or_else(invalid)?;
    if sha(market) != offer.marketplace_sha256 || sha(plugin) != offer.plugin_manifest_sha256 {
        return Err(invalid());
    }
    verify_manifests(offer, market, plugin)?;
    if let Some(bytes) = select("plugin.json") {
        let portable = crate::policy::json::parse(bytes)?;
        if portable["$schema"]
            .as_str()
            .is_some_and(|schema| schema.starts_with("https://agent-plugins.org/schemas/"))
        {
            return Err(invalid());
        }
    }
    let mut files: Files = contents
        .iter()
        .map(|(name, executable, data)| ((*name).into(), *executable, sha(data)))
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

fn verify_manifests(offer: &Offer, market: &[u8], plugin: &[u8]) -> Result<()> {
    let market = crate::policy::json::parse(market)?;
    let plugin = crate::policy::json::parse(plugin)?;
    let [member] = market["plugins"].as_array().ok_or_else(invalid)?.as_slice() else {
        return Err(invalid());
    };
    let source = &member["source"];
    let url = offer.url()?;
    if market["name"] != offer.marketplace
        || member["name"] != offer.plugin
        || source["source"] != "url"
        || source["ref"] != offer.payload_ref
        || !source["path"].is_null()
        || !source["sha"].is_null()
        || !(source["url"] == url || source["url"].as_str() == url.strip_suffix(".git"))
        || plugin["name"] != offer.plugin
        || plugin["version"] != offer.declared_version
    {
        return Err(invalid());
    }
    Ok(())
}

#[derive(Debug)]
struct GitFile {
    path: String,
    object: ObjectId,
    executable: bool,
    size: usize,
}

fn parse_listing(raw: &[u8]) -> Result<Vec<GitFile>> {
    let raw = raw.strip_suffix(&[0]).ok_or_else(invalid)?;
    let mut names = BTreeMap::new();
    let mut total = 0usize;
    let mut files = Vec::new();
    for row in raw.split(|b| *b == 0) {
        let row = std::str::from_utf8(row).map_err(|_| invalid())?;
        let (head, path) = row.split_once('\t').ok_or_else(invalid)?;
        let parts: Vec<_> = head.split_ascii_whitespace().collect();
        let [mode @ ("100644" | "100755"), "blob", object, size] = parts.as_slice() else {
            return Err(invalid());
        };
        let size = size.parse::<usize>().map_err(|_| invalid())?;
        total = total.checked_add(size).ok_or_else(invalid)?;
        if files.len() == 10_000 || total > MAX_TREE_BYTES || path == "." || !path.is_ascii() {
            return Err(invalid());
        }
        RelativePath::parse(path)?;
        for (index, _) in path.match_indices('/') {
            validate_name(&mut names, &path[..index], false)?;
        }
        validate_name(&mut names, path, true)?;
        files.push(GitFile {
            path: path.into(),
            object: ObjectId::parse(object)?,
            executable: *mode == "100755",
            size,
        });
    }
    Ok(files)
}

fn validate_name(
    names: &mut BTreeMap<String, (String, bool)>,
    path: &str,
    file: bool,
) -> Result<()> {
    let folded = path.to_ascii_lowercase();
    if let Some((existing, regular)) = names.get(&folded) {
        if *regular || file || existing != path {
            return Err(invalid());
        }
    } else {
        names.insert(folded, (path.into(), file));
    }
    Ok(())
}

fn parse_batch<'a, 'b>(
    listing: &'a [GitFile],
    mut rest: &'b [u8],
) -> Result<Vec<(&'a GitFile, &'b [u8])>> {
    let mut contents = Vec::with_capacity(listing.len());
    for file in listing {
        let boundary = rest.iter().position(|b| *b == b'\n').ok_or_else(invalid)?;
        let (header, following) = rest.split_at(boundary);
        let following = &following[1..];
        let expected = format!("{} blob {}", file.object.as_str(), file.size);
        let (blob, following) = following.split_at_checked(file.size).ok_or_else(invalid)?;
        if header != expected.as_bytes() {
            return Err(invalid());
        }
        rest = following.strip_prefix(b"\n").ok_or_else(invalid)?;
        contents.push((file, blob));
    }
    if !rest.is_empty() {
        return Err(invalid());
    }
    Ok(contents)
}

#[cfg(test)]
#[path = "../tests/unit/codex_install_source.rs"]
mod tests;
