use crate::{
    capability::link_inventory::Reader,
    context::Context,
    error::{Error, Result},
    platform::fs::{Directory, EntryKind},
    policy::governance::DirectoryBinding,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::PathBuf};
fn invalid() -> Error {
    Error::attention(
        "source_plugin_scope_changed",
        "原生插件登记范围未知或在来源准备期间改变",
    )
}
pub struct Observation {
    ctx: Context,
    home: DirectoryBinding,
    profiles: Vec<DirectoryBinding>,
    reader: Reader,
    pub unsupported: Vec<Value>,
}
fn profiles(ctx: &Context) -> Result<Vec<PathBuf>> {
    let home = Directory::open(&ctx.home)?;
    let mut roots = BTreeSet::new();
    for (count, item) in home.names()?.enumerate() {
        if count > 5000 {
            return Err(invalid());
        }
        let item = item.map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
        let name = item.file_name();
        let Ok(name) = name.to_str() else {
            continue;
        };
        if name != ".claude" && !name.starts_with(".claude-") {
            continue;
        }
        let metadata = home
            .metadata(std::ffi::OsStr::new(name))?
            .ok_or_else(invalid)?;
        if !matches!(metadata.kind, EntryKind::Directory | EntryKind::Symlink) {
            return Err(invalid());
        }
        roots.insert(ctx.home.join(name));
    }
    if let Some(path) = &ctx.harness.claude {
        crate::policy::paths::lexical(path)?;
        roots.insert(path.clone());
    }
    if roots.len() > 64 {
        return Err(invalid());
    }
    home.verify()?;
    Ok(roots.into_iter().collect())
}
pub fn observe(ctx: &Context) -> Result<Observation> {
    let home = Directory::open(&ctx.home)?;
    let home = crate::capability::capability_store::binding(&home, &ctx.home)?;
    let mut observation = Observation {
        ctx: ctx.clone(),
        home,
        profiles: vec![],
        reader: Reader::new(),
        unsupported: vec![],
    };
    for path in profiles(ctx)? {
        let root = Directory::open(&path)?;
        observation
            .profiles
            .push(crate::capability::capability_store::binding(&root, &path)?);
        let registry = path.join("plugins/installed_plugins.json");
        let Some(raw) = observation.reader.regular(&registry)? else {
            continue;
        };
        let profile_id = crate::domain::resource::id("agent", &("claude", &path))?;
        match crate::policy::catalog::resources::plugin_book(&raw) {
            Ok(book)=>for (name,records) in book.plugins {
                if records.is_empty() {observation.unsupported.push(json!({"id":format!("plugin:{name}"),"profile":profile_id,"reason":"unknown_native_plugin_registration"}));}
                for record in records {
                    let id=crate::domain::resource::id("capability",&("claude-plugin",&profile_id,&name,&record.scope,&record.project))?;
                    observation.unsupported.push(json!({"id":id,"native_id":name,"profile":profile_id,"scope":record.scope,"reason":"unsupported_native_plugin_source"}));
                }
            },
            Err(error)=>observation.unsupported.push(json!({"id":"source:native-plugins","profile":profile_id,"reason":"unknown_native_plugin_registry","diagnostic":error.code})),
        }
    }
    observation.verify()?;
    Ok(observation)
}
impl Observation {
    pub fn verify(&self) -> Result<()> {
        crate::capability::capability_store::open(
            &crate::capability::rules::capability_tx::Namespace {
                ancestor: self.home.clone(),
                remaining: vec![".probe".into()],
            },
        )?;
        if profiles(&self.ctx)?
            != self
                .profiles
                .iter()
                .map(|binding| binding.requested.clone())
                .collect::<Vec<_>>()
        {
            return Err(invalid());
        }
        for binding in &self.profiles {
            crate::capability::capability_store::open(
                &crate::capability::rules::capability_tx::Namespace {
                    ancestor: binding.clone(),
                    remaining: vec![".probe".into()],
                },
            )?;
        }
        self.reader.verify()
    }
}

#[cfg(test)]
#[path = "../../tests/unit/source_plugins.rs"]
mod tests;
