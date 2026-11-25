use crate::{
    agent::resource_agents::{self, InstallTarget},
    context::Context,
    error::{Error, Result},
};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InitialActivation {
    Enabled,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    VerifySource,
    RegisterMarketplace,
    InstallPlugin,
    CheckActivation,
    Publish,
}

pub(crate) struct Selection {
    pub target: Box<InstallTarget>,
    pub registered: bool,
}

enum Outcome {
    Complete(Value),
    Pending { raw: Value, continuation: bool },
}

impl Outcome {
    fn from_adapter(raw: Value, target: &InstallTarget) -> Self {
        let continuation = crate::policy::cli::continuation(&raw).is_some();
        if raw["journal"]["phase"] == "succeeded"
            && raw["targetName"].as_str() == Some(&target.name)
            && raw["profile"].is_string()
            && raw["profile"].as_str() == target.directory.canonical.to_str()
            && !continuation
        {
            return Self::Complete(raw);
        }
        Self::Pending { raw, continuation }
    }
}

pub(crate) enum Event<'a> {
    Reconciled,
    Complete(&'a Value),
    Stopped {
        target: &'a InstallTarget,
        completed: usize,
        remaining: &'a [Selection],
        pending: Option<Pending<'a>>,
    },
    BatchComplete(usize),
}

pub(crate) struct Pending<'a> {
    pub raw: &'a Value,
    pub continuation: bool,
}

pub(crate) fn execute(
    ctx: &Context,
    selected: &[Selection],
    confirm: impl FnOnce() -> Result<InitialActivation>,
    mut install: impl FnMut(InitialActivation, InstallTarget) -> Result<Value>,
    mut observe: impl FnMut(Event<'_>),
) -> Result<Value> {
    if selected.is_empty() {
        return Err(Error::new("plugin_target_unsupported", "请选择安装位置。"));
    }
    check_targets(selected)?;
    let activation = confirm()?;
    check_targets(selected)?;
    if crate::operations::recover_before_write_checked(ctx, None, |journal| {
        Ok(crate::agent::plugin_target::recovery_target(journal)?
            .is_some_and(|original| selected.iter().any(|item| *item.target == original)))
    })?
    .is_some()
    {
        observe(Event::Reconciled);
    }

    let mut results = Vec::new();
    for (index, item) in selected.iter().enumerate() {
        let target = item.target.as_ref();
        let installed = (|| {
            target.open()?;
            if !item.registered {
                resource_agents::enroll(ctx, target)?;
            }
            // The adapter verifies and reuses already published results under its write lock.
            install(activation, target.clone()).map(|raw| Outcome::from_adapter(raw, target))
        })();
        let remaining = &selected[index + 1..];
        match installed {
            Ok(Outcome::Complete(raw)) => {
                observe(Event::Complete(&raw));
                results.push(raw);
            }
            Ok(Outcome::Pending {
                mut raw,
                continuation,
            }) => {
                observe(Event::Stopped {
                    target,
                    completed: results.len(),
                    remaining,
                    pending: Some(Pending {
                        raw: &raw,
                        continuation,
                    }),
                });
                if selected.len() == 1 {
                    if let Some(fields) = raw.as_object_mut() {
                        fields.insert("incomplete".into(), Value::Bool(true));
                        return Ok(raw);
                    }
                    return Ok(json!({"incomplete":true,"result":raw}));
                }
                results.push(raw);
                return Ok(json!({"incomplete":true,"results":results}));
            }
            Err(error) => {
                observe(Event::Stopped {
                    target,
                    completed: results.len(),
                    remaining,
                    pending: None,
                });
                return Err(error);
            }
        }
    }
    if selected.len() == 1 {
        return Ok(results.pop().unwrap_or_default());
    }
    observe(Event::BatchComplete(results.len()));
    Ok(json!({"results":results}))
}

fn check_targets(selected: &[Selection]) -> Result<()> {
    for item in selected {
        item.target.open()?;
    }
    Ok(())
}
