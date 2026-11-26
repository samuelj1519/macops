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
    Skipped(Value),
    Pending { raw: Value, continuation: bool },
}

impl Outcome {
    fn from_adapter(raw: Value, target: &InstallTarget) -> Self {
        let continuation = crate::policy::cli::continuation(&raw).is_some();
        let same_target = raw["targetName"].as_str() == Some(&target.name)
            && raw["profile"].is_string()
            && raw["profile"].as_str() == target.directory.canonical.to_str();
        if same_target && !continuation {
            if raw["journal"]["phase"] == "succeeded" {
                return Self::Complete(raw);
            }
            if raw["skipped"] == true {
                return Self::Skipped(raw);
            }
        }
        Self::Pending { raw, continuation }
    }
}

pub(crate) enum Event<'a> {
    Reconciled(&'a Value),
    Complete(&'a Value),
    Skipped(&'a InstallTarget, &'a Value),
    Stopped {
        target: &'a InstallTarget,
        completed: usize,
        remaining: &'a [Selection],
        pending: Option<Pending<'a>>,
    },
    BatchComplete {
        installed: usize,
        skipped: usize,
    },
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
    // The original journal owns its destination independently of this selection.
    if let Some(recovered) = crate::operations::recover_before_write(ctx, None)? {
        observe(Event::Reconciled(&recovered.result));
    }

    let mut results = Vec::new();
    let mut completed = 0;
    let mut skipped = 0;
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
                completed += 1;
                results.push(raw);
            }
            Ok(Outcome::Skipped(raw)) => {
                observe(Event::Skipped(target, &raw));
                skipped += 1;
                results.push(raw);
            }
            Ok(Outcome::Pending {
                mut raw,
                continuation,
            }) => {
                observe(Event::Stopped {
                    target,
                    completed,
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
                    completed,
                    remaining,
                    pending: None,
                });
                return Err(error);
            }
        }
    }
    if selected.len() == 1 {
        let mut result = results.pop().unwrap_or_default();
        if skipped != 0 {
            result["incomplete"] = Value::Bool(true);
        }
        return Ok(result);
    }
    observe(Event::BatchComplete {
        installed: completed,
        skipped,
    });
    Ok(json!({"results":results, "incomplete":skipped != 0}))
}

fn check_targets(selected: &[Selection]) -> Result<()> {
    for item in selected {
        item.target.open()?;
    }
    Ok(())
}
