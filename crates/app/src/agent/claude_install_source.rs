use crate::{
    agent::{
        codex_install_source::{Files, VerifiedFile, sha, with_verified_files},
        native_plugins::valid_name,
    },
    error::{Error, Result},
    platform::fs::Directory,
    policy::{
        source_git::{ObjectId, Repository},
        source_tree::{Slot, Tree},
    },
    source::{
        rules::source_payload::{Entry, Payload},
        source_git::Git,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

pub(crate) const MARKET: &str = ".claude-plugin/marketplace.json";
pub(crate) const PLUGIN: &str = ".claude-plugin/plugin.json";

fn invalid() -> Error {
    Error::attention(
        "claude_install_source_unverified",
        "无法将 Claude Code 插件安装绑定到已核验提交",
    )
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Offer {
    pub repository: String,
    pub commit: String,
    pub marketplace: String,
    pub plugin: String,
    pub version: String,
    marketplace_sha256: String,
    plugin_sha256: String,
}

impl Offer {
    pub fn parse(value: &Value) -> Result<Self> {
        let offer: Self = serde_json::from_value(value.clone())?;
        offer.validate()?;
        Ok(offer)
    }

    pub fn validate(&self) -> Result<()> {
        Repository::parse(&self.repository)?;
        ObjectId::parse(&self.commit)?;
        if [&self.marketplace, &self.plugin, &self.version]
            .into_iter()
            .all(|label| valid_name(label))
            && [&self.marketplace_sha256, &self.plugin_sha256]
                .into_iter()
                .all(|digest| crate::policy::config_local::valid_digest(digest))
        {
            Ok(())
        } else {
            Err(invalid())
        }
    }

    pub fn identify(
        repository: &str,
        commit: &ObjectId,
        market: &[u8],
        plugin: &[u8],
    ) -> Result<Self> {
        let registry = crate::policy::json::parse(market)?;
        let manifest = crate::policy::json::parse(plugin)?;
        let [member] = registry["plugins"]
            .as_array()
            .ok_or_else(invalid)?
            .as_slice()
        else {
            return Err(invalid());
        };
        if !matches!(member["source"].as_str(), Some("." | "./"))
            || member["name"] != manifest["name"]
        {
            return Err(invalid());
        }
        let offer = Self {
            repository: Repository::parse(repository)?.key().into(),
            commit: commit.as_str().into(),
            marketplace: registry["name"].as_str().ok_or_else(invalid)?.into(),
            plugin: manifest["name"].as_str().ok_or_else(invalid)?.into(),
            version: manifest["version"].as_str().ok_or_else(invalid)?.into(),
            marketplace_sha256: sha(market),
            plugin_sha256: sha(plugin),
        };
        offer.validate()?;
        Ok(offer)
    }

    pub fn native_id(&self) -> String {
        format!("{}@{}", self.plugin, self.marketplace)
    }

    pub fn url(&self) -> Result<String> {
        Ok(Repository::parse(&self.repository)?.url())
    }
}

pub(crate) fn checkout(offer: &Offer, allocation: &Directory) -> Result<(Slot, Tree, Files)> {
    checkout_using(offer, allocation, |git, repository, commit| {
        git.fetch_full_mirror(repository, commit, "macops-pinned")
            .map(|(tree, _)| tree)
    })
}

fn checkout_using(
    offer: &Offer,
    allocation: &Directory,
    fetch: impl FnOnce(&Git, &Repository, &ObjectId) -> Result<ObjectId>,
) -> Result<(Slot, Tree, Files)> {
    offer.validate()?;
    let scratch = tempfile::Builder::new()
        .prefix("claude-source-")
        .tempdir_in(&allocation.path)?;
    let mut git = Git::create(&scratch.path().join("git"))?;
    git.set_storage_budget(256 * 1024 * 1024)?;
    let tree = fetch(
        &git,
        &Repository::parse(&offer.repository)?,
        &ObjectId::parse(&offer.commit)?,
    )?;
    with_verified_files(&git, &tree, |files| materialize(offer, allocation, files))
}

pub(crate) fn materialize(
    offer: &Offer,
    allocation: &Directory,
    files: &[VerifiedFile<'_>],
) -> Result<(Slot, Tree, Files)> {
    let (payload, files) = material(offer, files)?;
    let (slot, tree) =
        crate::source::source_payload::materialize_tree(allocation, "checkout", &payload, 0o700)?;
    Ok((slot, tree, files))
}

fn material(offer: &Offer, files: &[VerifiedFile<'_>]) -> Result<(Payload, Files)> {
    let find = |name| {
        files
            .iter()
            .find(|(path, _, _)| *path == name)
            .map(|(_, _, bytes)| *bytes)
            .ok_or_else(invalid)
    };
    let picked = Offer::identify(
        &offer.repository,
        &ObjectId::parse(&offer.commit)?,
        find(MARKET)?,
        find(PLUGIN)?,
    )?;
    if crate::domain::fingerprint(&picked)? != crate::domain::fingerprint(offer)? {
        return Err(invalid());
    }
    let mut payload = Payload::default();
    let mut digests: Files = vec![];
    for (path, executable, bytes) in files {
        for parent in Path::new(path)
            .ancestors()
            .skip(1)
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            let name = parent.to_str().ok_or_else(invalid)?;
            payload
                .entries
                .entry(name.into())
                .or_insert(Entry::Directory { mode: 0o700 });
        }
        payload.entries.insert(
            (*path).into(),
            Entry::File {
                mode: if *executable { 0o700 } else { 0o600 },
                bytes: bytes.to_vec(),
            },
        );
        digests.push(((*path).into(), *executable, sha(bytes)));
    }
    digests.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((payload, digests))
}

#[cfg(test)]
#[path = "../../tests/unit/claude_install_source.rs"]
mod tests;
