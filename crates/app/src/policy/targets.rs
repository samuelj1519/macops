use crate::{
    domain::resource,
    domain::{Action, Journal, Kind, Observation, Package, Provider},
    error::{Error, Result},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NpmTarget {
    pub npm: PathBuf,
    pub prefix: PathBuf,
    pub name: String,
}

pub fn npm_id(prefix: &Path, name: &str) -> Result<String> {
    resource::id("application", &("npm", prefix, name))
}

pub fn descriptor(package: &Package) -> Result<Option<NpmTarget>> {
    let Provider::Command {
        query,
        available,
        install,
        upgrade,
        uninstall,
        ..
    } = &package.provider
    else {
        return Ok(None);
    };
    if query.args.first().map(String::as_str) != Some("__npm_observe") {
        return Ok(None);
    }
    if available.is_some()
        || install.is_some()
        || upgrade.is_some()
        || uninstall.is_some()
        || package.kind != Kind::Cli
    {
        return Err(Error::new(
            "invalid_observer_recipe",
            "原生观察recipe不得携带写命令",
        ));
    }
    let args = &query.args;
    if args.len() != 7 || args[1] != "--npm" || args[3] != "--prefix" || args[5] != "--name" {
        return Err(Error::new("invalid_observer_recipe", "原生观察参数不匹配"));
    }
    let target = NpmTarget {
        npm: crate::policy::paths::lexical(Path::new(&args[2]))?,
        prefix: crate::policy::paths::lexical(Path::new(&args[4]))?,
        name: args[6].clone(),
    };
    if !crate::policy::parsers::registry_name(&target.name)
        || query.program == target.npm
        || npm_id(&target.prefix, &target.name).ok().as_deref() != Some(package.id.as_str())
    {
        return Err(Error::new(
            "invalid_observer_recipe",
            "观察recipe身份不匹配",
        ));
    }
    Ok(Some(target))
}

pub fn npm_observer(package: &Package) -> Result<bool> {
    Ok(matches!(package.provider, Provider::NpmObserved { .. }) || descriptor(package)?.is_some())
}

pub fn observer(package: &Package) -> Result<bool> {
    Ok(npm_observer(package)?
        || matches!(
            package.provider,
            Provider::Uv {
                binding: Some(_),
                ..
            } | Provider::UvObserved { .. }
                | Provider::ConfigObserved { .. }
                | Provider::CapabilityObserved { .. }
        ))
}

pub fn validate_journal(journal: &Journal) -> Result<()> {
    if !npm_observer(&journal.plan.package)? {
        return if matches!(journal.plan.before, Observation::NativePackage { .. }) {
            Err(Error::new(
                "invalid_observer_journal",
                "原生观察不能转为通用写recipe",
            ))
        } else {
            Ok(())
        };
    }
    if journal.plan.action != Action::Adopt
        || journal.plan.command.is_some()
        || !match (&journal.plan.package.provider, &journal.plan.query) {
            (Provider::Command { query, .. }, Some(crate::domain::QuerySpec::Command(spec))) => {
                query == spec
            }
            (Provider::NpmObserved { .. }, Some(crate::domain::QuerySpec::LocalNpm(_))) => true,
            _ => false,
        }
        || journal.plan.delegated
        || !journal.plan.synchronous
        || !matches!(journal.plan.before, Observation::NativePackage { .. })
    {
        return Err(Error::new(
            "invalid_observer_journal",
            "原生观察recipe只支持采纳，不能执行软件写命令",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/targets.rs"]
mod tests;

pub fn merge_views(
    items: Vec<crate::domain::resource::Resource>,
) -> (Vec<crate::domain::resource::Resource>, bool) {
    use crate::domain::resource::{Data, Management, State};
    use std::collections::btree_map::Entry;
    let mut map = BTreeMap::<String, (crate::domain::resource::Resource, BTreeSet<String>)>::new();
    for mut incoming in items {
        let recipe = match &incoming.data {
            Data::Application { recipe_ref, .. } => recipe_ref.clone(),
            _ => None,
        };
        match map.entry(incoming.id.clone()) {
            Entry::Vacant(slot) => {
                slot.insert((incoming, recipe.into_iter().collect()));
            }
            Entry::Occupied(mut slot) => {
                let (old, recipes) = slot.get_mut();
                recipes.extend(recipe);
                let adopted = old.management == Management::Adopted
                    || incoming.management == Management::Adopted;
                let native = incoming.management == Management::Native
                    && old.management != Management::Native;
                let mut aliases = old.aliases.clone();
                aliases.append(&mut incoming.aliases);
                aliases.extend([old.name.clone(), incoming.name.clone()]);
                if native {
                    *old = incoming;
                }
                aliases.sort();
                aliases.dedup();
                old.aliases = aliases;
                if adopted {
                    old.management = Management::Adopted;
                }
            }
        }
    }
    let mut conflict = false;
    let resources = map
        .into_values()
        .map(|(mut item, recipes)| {
            if let Data::Application { recipe_ref, .. } = &mut item.data {
                if recipes.len() > 1 {
                    *recipe_ref = None;
                    item.status = State::Unknown;
                    conflict = true;
                } else {
                    *recipe_ref = recipes.into_iter().next();
                }
            }
            item
        })
        .collect();
    (resources, conflict)
}

pub fn baseline(before: &Observation, current: Observation) -> crate::domain::resource::Baseline {
    use crate::domain::resource::{Baseline, Comparison};
    let comparable = (matches!(before, Observation::UvTool { .. })
        && matches!(
            current,
            Observation::UvTool { .. } | Observation::Missing { .. }
        ))
        || (matches!(before, Observation::NativePackage { .. })
            && matches!(
                current,
                Observation::NativePackage { .. } | Observation::Missing { .. }
            ))
        || (matches!(before, Observation::ConfigFile { .. })
            && matches!(
                current,
                Observation::ConfigFile { .. } | Observation::Missing { .. }
            ))
        || (matches!(before, Observation::CapabilityPack { .. })
            && matches!(
                current,
                Observation::CapabilityPack { .. } | Observation::Missing { .. }
            ));
    let comparison = if comparable {
        if before == &current {
            Comparison::Same
        } else {
            Comparison::Different
        }
    } else {
        Comparison::Unknown
    };
    Baseline {
        receipt: before.clone(),
        current,
        comparison,
    }
}
