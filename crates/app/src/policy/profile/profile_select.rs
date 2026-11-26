use crate::{
    capability::rules::delivery::{Profile, Selection},
    error::{Error, Result},
    policy::profile::profile_fields::ConfigProfile,
};
use std::collections::BTreeSet;
#[derive(Debug)]
pub struct Selected {
    pub links: Option<Selection>,
    pub profiles: BTreeSet<ConfigProfile>,
}
fn invalid() -> Error {
    Error::attention("invalid_profile_selection", "profile 配置选择重复或不支持")
}
pub fn split(request: Selection) -> Result<Selected> {
    match request {
        Selection::All => Ok(Selected {
            links: Some(Selection::All),
            profiles: Profile::ALL
                .into_iter()
                .map(ConfigProfile::Claude)
                .chain(std::iter::once(ConfigProfile::Codex))
                .collect(),
        }),
        Selection::Explicit(keys) => {
            let mut profiles = BTreeSet::new();
            let mut links = vec![];
            for key in keys {
                if let Some(profile) = key.strip_prefix("profile:") {
                    let profile = if profile == "codex" {
                        ConfigProfile::Codex
                    } else {
                        ConfigProfile::Claude(Profile::parse(profile)?)
                    };
                    if !profiles.insert(profile) {
                        return Err(invalid());
                    }
                } else {
                    links.push(key);
                }
            }
            if profiles.is_empty() && links.is_empty() {
                return Err(invalid());
            }
            Ok(Selected {
                links: if links.is_empty() {
                    None
                } else {
                    Some(Selection::Explicit(links))
                },
                profiles,
            })
        }
    }
}
#[cfg(test)]
#[path = "../../../tests/unit/profile_select_policy.rs"]
mod tests;
