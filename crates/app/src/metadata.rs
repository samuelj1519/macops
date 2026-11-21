use crate::{
    cli::Credential,
    context::Context,
    domain::CommandSpec,
    error::{Error, Result},
    paths,
    policy::metadata::{self as policy, Profile},
    process,
};
use serde_json::{Value, json};
use std::time::Duration;

pub fn credentials(ctx: &Context, command: &Credential) -> Value {
    match command {
        Credential::Add { profile, alias } | Credential::Activate { profile, alias } => {
            let verb = if matches!(command, Credential::Add { .. }) {
                "add"
            } else {
                "activate"
            };
            json!({"backend":"rust","command":{"program":ctx.home.join(".local/bin/claudectl"),"args":["credential",verb,profile.name(),alias]},"account":format!("claude-{}@{alias}",profile.name()),"service":policy::KEYCHAIN_SERVICE,"executed":false,"instruction":"运行所列命令，由 claudectl 交互处理 Keychain；macops 不接受密钥值"})
        }
        Credential::List | Credential::Check { .. } => {
            let profiles = match command {
                Credential::Check {
                    profile: Some(profile),
                } => vec![*profile],
                _ => policy::PROFILES.to_vec(),
            };
            json!(
                profiles
                    .into_iter()
                    .map(|profile| credential_profile(ctx, profile))
                    .collect::<Vec<_>>()
            )
        }
    }
}

fn credential_profile(ctx: &Context, profile: Profile) -> Value {
    let path = ctx
        .home
        .join(format!(".claude-{}/settings.json", profile.name()));
    let mut row = json!({"profile":profile.name(),"settings":path,"service":policy::KEYCHAIN_SERVICE,"account":null});
    match credential_account(ctx, &path, profile) {
        Ok(None) => {
            row["status"] = json!("missing");
            row["reason"] = json!("settings_missing");
        }
        Ok(Some(account)) => {
            row["account"] = json!(account);
            match credential_probe(ctx, &account) {
                Ok(probe) => {
                    row["status"] = json!(policy::keychain_status(probe.code, probe.uncertain));
                    row["nativeExit"] = json!(probe.code);
                }
                Err(error) => {
                    row["status"] = json!("unknown");
                    row["reason"] = json!(error.code);
                }
            }
        }
        Err(error) => {
            row["status"] = json!("unknown");
            row["reason"] = json!(error.code);
        }
    }
    row
}

fn credential_account(
    ctx: &Context,
    path: &std::path::Path,
    profile: Profile,
) -> Result<Option<String>> {
    paths::read_path(path)?;
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_file() => {
            return Err(Error::new(
                "invalid_profile_settings",
                "profile settings 必须是普通文件",
            ));
        }
        Ok(_) => (),
    }
    let value: Value = paths::regular_json(path)
        .map_err(|_| Error::new("invalid_profile_settings", "profile settings 无法解析"))?;
    let helper = ctx.home.join(".local/libexec/claude-key-helper");
    policy::active_account(&value, &helper, profile)
        .map(Some)
        .ok_or_else(|| {
            Error::new(
                "invalid_profile_settings",
                "apiKeyHelper 与声明的 profile 不匹配",
            )
        })
}

fn credential_probe(ctx: &Context, account: &str) -> Result<process::Probe> {
    let program = process::executable("security")?;
    if ctx.isolated && !paths::read_path(&program)?.starts_with(&ctx.home) {
        return Err(Error::new(
            "fixture_security_refused",
            "隔离 HOME 不调用宿主的 Keychain 工具",
        ));
    }
    process::probe(
        &ctx.scope,
        &CommandSpec {
            program,
            args: vec![
                "find-generic-password".into(),
                "-s".into(),
                policy::KEYCHAIN_SERVICE.into(),
                "-a".into(),
                account.into(),
            ],
        },
        Duration::from_secs(ctx.timeout_secs.min(10)),
    )
}
