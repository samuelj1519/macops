use super::{Prepared, Step, invalid, physical};
use crate::{
    agent::native_plugins_files::Passive,
    domain::{CapabilityKind, FileId, fingerprint},
    error::Result,
    platform::fs::Directory,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::ffi::OsStr;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Projection {
    pub(super) enabled: Option<bool>,
    pub(super) market: String,
    pub(super) plugins: Option<String>,
    pub(super) payload: Option<FileId>,
}

pub(super) fn file(root: &Directory, name: &str) -> Result<Option<Value>> {
    let name = OsStr::new(name);
    if root.metadata(name)?.is_none() {
        return Ok(None);
    }
    let (bytes, _) = root.regular_config_snapshot(name, 1024 * 1024)?;
    crate::policy::json::parse(&bytes).map(Some)
}

fn source(p: &Prepared) -> Value {
    json!({"source":"directory","path":p.checkout()})
}

fn marketplace_matches(p: &Prepared, entry: &Value) -> bool {
    entry["source"] == source(p)
        && entry["installLocation"].as_str() == p.checkout().to_str()
        && entry["lastUpdated"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
        && entry.as_object().is_some_and(|data| data.len() == 3)
}

fn verify_installed_record(p: &Prepared, installed: &Value) -> Result<()> {
    let id = p.offer.native_id();
    let [record] = installed["plugins"][&id]
        .as_array()
        .ok_or_else(invalid)?
        .as_slice()
    else {
        return Err(invalid());
    };
    if installed["version"] != 2
        || installed.as_object().is_none_or(|root| root.len() != 2)
        || record["scope"] != "user"
        || record["version"] != p.offer.version
        || record["installPath"].as_str() != p.payload().to_str()
        || record.as_object().is_none_or(|fields| fields.len() != 5)
        || ["installedAt", "lastUpdated"]
            .iter()
            .any(|key| record[key].as_str().is_none_or(str::is_empty))
    {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn published(
    p: &Prepared,
    expected_payload: &FileId,
    reader: &mut Passive,
) -> Result<(Option<String>, String)> {
    let profile = p.profile();
    let (settings, settings_stamp) = reader.stamped_file(&profile.join("settings.json"))?;
    let settings_stamp = settings_stamp.clone();
    let settings_bytes = settings.ok_or_else(invalid)?;
    let settings = crate::policy::json::parse(&settings_bytes)?;
    let selected = &p.offer.marketplace;
    let id = p.offer.native_id();
    if settings["extraKnownMarketplaces"][selected]["source"] != source(p)
        || settings["enabledPlugins"][&id].as_bool().is_none()
    {
        return Err(invalid());
    }
    let plugins = profile.join("plugins");
    let (market, market_stamp) = reader.stamped_file(&plugins.join("known_marketplaces.json"))?;
    let market_stamp = market_stamp.clone();
    let market = crate::policy::json::parse(&market.ok_or_else(invalid)?)?;
    if !marketplace_matches(p, &market[selected]) {
        return Err(invalid());
    }
    let (installed, install_stamp) =
        reader.stamped_file(&plugins.join("installed_plugins.json"))?;
    let installed = crate::policy::json::parse(&installed.ok_or_else(invalid)?)?;
    verify_installed_record(p, &installed)?;
    let evidence = fingerprint(&(settings_stamp, market_stamp, install_stamp))?;
    let payload = reader.directory(&p.payload())?.ok_or_else(invalid)?;
    reader.projected_tree(&p.payload(), None)?;
    if payload.identity() != expected_payload
        || crate::capability::capability_local::plugin_payload_files(
            &payload,
            CapabilityKind::ClaudePlugin,
        )? != p.files
    {
        return Err(invalid());
    }
    Ok((
        crate::policy::catalog::resources::claude_model(&settings_bytes)?.selected,
        evidence,
    ))
}

pub(super) fn empty(p: &Prepared) -> Result<()> {
    let profile = physical(&p.profile())?;
    if file(&profile, "settings.json")? != Some(json!({}))
        || profile.metadata(OsStr::new("plugins"))?.is_some()
    {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn observe(p: &Prepared, step: &Step) -> Result<Projection> {
    let profile = physical(&p.profile())?;
    let plugins = profile.child(OsStr::new("plugins"))?;
    let market = file(&plugins, "known_marketplaces.json")?.ok_or_else(invalid)?;
    let market_name = &p.offer.marketplace;
    let entry = &market[market_name];
    if !marketplace_matches(p, entry) || market.as_object().is_none_or(|rows| rows.len() != 1) {
        return Err(invalid());
    }
    let mut settings = json!({"extraKnownMarketplaces":{market_name:{"source":source(p)}}});
    let actual = file(&profile, "settings.json")?.ok_or_else(invalid)?;
    if matches!(step, Step::Market) {
        if actual != settings
            || file(&plugins, "installed_plugins.json")?.is_some()
            || plugins.metadata(OsStr::new("cache"))?.is_some()
        {
            return Err(invalid());
        }
        return Ok(Projection {
            enabled: None,
            market: fingerprint(&market)?,
            plugins: None,
            payload: None,
        });
    }
    let id = p.offer.native_id();
    let enabled = actual["enabledPlugins"][&id]
        .as_bool()
        .ok_or_else(invalid)?;
    settings["enabledPlugins"] = json!({id.as_str():enabled});
    if actual != settings || (matches!(step, Step::Activation { .. }) && enabled != p.enabled) {
        return Err(invalid());
    }
    let installed = file(&plugins, "installed_plugins.json")?.ok_or_else(invalid)?;
    verify_installed_record(p, &installed)?;
    if installed["plugins"]
        .as_object()
        .is_none_or(|rows| rows.len() != 1)
    {
        return Err(invalid());
    }
    let payload = physical(&p.payload())?;
    if crate::capability::capability_local::plugin_payload_files(
        &payload,
        CapabilityKind::ClaudePlugin,
    )? != p.files
    {
        return Err(invalid());
    }
    profile.verify()?;
    plugins.verify()?;
    Ok(Projection {
        enabled: Some(enabled),
        market: fingerprint(&market)?,
        plugins: Some(fingerprint(&installed)?),
        payload: Some(payload.identity().clone()),
    })
}
