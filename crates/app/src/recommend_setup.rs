use crate::{
    context::Context,
    error::{Error, Result},
    native_skills_files::{regular, write_cas},
    policy::recommend_model,
    store::Store,
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io, path::Path};

const PROFILE: &str = "recommend-model.json";
const PENDING: &str = "recommend-model-pending.json";

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Profile {
    schema_version: u32,
    pub(crate) endpoint: String,
    pub(crate) model: String,
    account: String,
    pub(crate) enabled: bool,
}

fn invalid() -> Error {
    Error::attention("model_setup_invalid", "需要有效的 HTTPS Base URL 和模型名")
}

fn keychain_error() -> Error {
    Error::attention("model_keychain_unavailable", "未能完成钥匙串操作")
}

fn valid_piece(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
}

fn endpoint(value: &str) -> Result<String> {
    let base = value.trim().trim_end_matches('/');
    let rest = base.strip_prefix("https://").ok_or_else(invalid)?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = authority
        .split_once(':')
        .map_or((authority, None), |(host, port)| (host, Some(port)));
    if host.len() > 253
        || !host.split('.').all(|label| {
            valid_piece(label)
                && !label.contains('_')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
        || port.is_some_and(|value| {
            value.parse::<u16>().map_or(true, |port| port == 0) || value.starts_with('+')
        })
        || (!path.is_empty()
            && !path
                .split('/')
                .all(|part| valid_piece(part) && part != "." && part != ".."))
    {
        return Err(invalid());
    }
    let mut full = format!("https://{}", authority.to_ascii_lowercase());
    if !path.is_empty() {
        full.push('/');
        full.push_str(path);
    }
    if !full.ends_with("/chat/completions") {
        full.push_str("/chat/completions");
    }
    (full.len() <= 256).then_some(full).ok_or_else(invalid)
}

fn profile(base: &str, model: &str) -> Result<Profile> {
    let endpoint = endpoint(base)?;
    let model = model.trim();
    if model.len() > 128 || !model.split(['/', ':']).all(valid_piece) {
        return Err(invalid());
    }
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce).map_err(|_| keychain_error())?;
    Ok(Profile {
        schema_version: 1,
        endpoint,
        model: model.into(),
        account: format!("setup-{:x}", Sha256::digest(nonce)),
        enabled: true,
    })
}

impl Profile {
    pub(crate) fn service(&self, config: &Path) -> String {
        use std::os::unix::ffi::OsStrExt;
        let mut binding = Sha256::new();
        binding.update(config.as_os_str().as_bytes());
        binding.update([0]);
        binding.update(self.endpoint.as_bytes());
        format!("org.macops.recommend.endpoint.{:x}", binding.finalize())
    }

    pub(crate) fn account(&self) -> &str {
        &self.account
    }
}

fn optional_record<T: serde::de::DeserializeOwned>(bytes: Option<&[u8]>) -> Result<Option<T>> {
    Ok(bytes
        .map(crate::policy::json::parse)
        .transpose()?
        .map(serde_json::from_value::<Option<crate::policy::json::Object<T>>>)
        .transpose()?
        .flatten()
        .map(|object| object.0))
}

fn validate(p: &Profile) -> Result<()> {
    if p.schema_version != 1
        || p.endpoint != endpoint(&p.endpoint)?
        || p.account
            .strip_prefix("setup-")
            .is_none_or(|s| !crate::policy::config_local::valid_digest(s))
        || p.model.len() > 128
        || !p.model.split(['/', ':']).all(valid_piece)
    {
        return Err(invalid());
    }
    Ok(())
}

fn decode(bytes: Option<&[u8]>) -> Result<Option<Profile>> {
    let value = optional_record(bytes)?;
    if let Some(profile) = &value {
        validate(profile)?;
    }
    Ok(value)
}

pub(crate) fn read(ctx: &Context) -> Result<Option<Profile>> {
    decode(regular(&ctx.config.join(PROFILE))?.as_deref())
}

trait KeyStore {
    fn add(&self, profile: &Profile, token: &str) -> Result<()>;
    fn remove(&self, profile: &Profile) -> Result<()>;
    fn contains(&self, profile: &Profile) -> Result<bool>;
}

struct NativeKeychain<'a>(Option<&'a Path>);
impl KeyStore for NativeKeychain<'_> {
    fn add(&self, profile: &Profile, token: &str) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            use security_framework::os::macos::keychain::SecKeychain;
            let service = profile.service(self.0.ok_or_else(keychain_error)?);
            SecKeychain::default()
                .and_then(|keys| {
                    keys.add_generic_password(&service, profile.account(), token.as_bytes())
                })
                .map_err(|_| keychain_error())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (self.0, profile, token);
            Err(keychain_error())
        }
    }

    fn remove(&self, profile: &Profile) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            match security_framework::passwords::delete_generic_password(
                &profile.service(self.0.ok_or_else(keychain_error)?),
                profile.account(),
            ) {
                Ok(()) => Ok(()),
                Err(error) if error.code() == -25300 => Ok(()),
                Err(_) => Err(keychain_error()),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (self.0, profile);
            Err(keychain_error())
        }
    }
    fn contains(&self, profile: &Profile) -> Result<bool> {
        #[cfg(target_os = "macos")]
        {
            match security_framework::passwords::get_generic_password(
                &profile.service(self.0.ok_or_else(keychain_error)?),
                profile.account(),
            ) {
                Ok(_) => Ok(true),
                Err(error) if error.code() == -25300 => Ok(false),
                Err(_) => Err(keychain_error()),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (self.0, profile);
            Err(keychain_error())
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    profile: Profile,
    added: bool,
}

fn recover_pending(dir: &Path, current: Option<&Profile>, keys: &impl KeyStore) -> Result<()> {
    let path = dir.join(PENDING);
    let raw = regular(&path)?;
    if let Some(staged) = optional_record::<Pending>(raw.as_deref())? {
        validate(&staged.profile)?;
        if !staged.added && keys.contains(&staged.profile)? {
            return Err(Error::attention(
                "model_keychain_confirmation_pending",
                "原钥匙串写入待核对，已保留原配置和待处理记录",
            ));
        }
        if staged.added
            && current.is_none_or(|p| {
                p.account() != staged.profile.account() || p.endpoint != staged.profile.endpoint
            })
        {
            keys.remove(&staged.profile)?;
        }
        write_cas(&path, raw.as_deref(), b"null")?;
    }
    Ok(())
}

fn publish(
    ctx: &Context,
    before: Option<&[u8]>,
    new: &Profile,
    token: Option<&str>,
    keys: &impl KeyStore,
) -> Result<()> {
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    let target = ctx.config.join(PROFILE);
    if regular(&target)?.as_deref() != before {
        return Err(Error::attention(
            "model_setup_stale",
            "模型设置已改变，请重新打开设置",
        ));
    }
    recover_pending(&ctx.config, decode(before)?.as_ref(), keys)?;
    let encoded = serde_json::to_vec(new)?;
    let pending = ctx.config.join(PENDING);
    let mut completed = None;
    if let Some(token) = token {
        let previous = regular(&pending)?;
        let mut staged = Pending {
            profile: new.clone(),
            added: false,
        };
        let intent = serde_json::to_vec(&staged)?;
        write_cas(&pending, previous.as_deref(), &intent)?;
        keys.add(new, token)?;
        staged.added = true;
        let written = serde_json::to_vec(&staged)?;
        write_cas(&pending, Some(&intent), &written)?;
        completed = Some(written);
    }
    write_cas(&target, before, &encoded)?;
    if let Some(written) = completed {
        write_cas(&pending, Some(&written), b"null")?;
    }
    Ok(())
}

fn status(current: Option<&Profile>) -> Value {
    json!({
        "mode":if current.is_some_and(|p| p.enabled) {"remote"} else {"local"},
        "endpoint": current.map(|p| p.endpoint.as_str()),
        "model": current.map(|p| p.model.as_str())
    })
}

fn disable_profile(ctx: &Context, before: Option<&[u8]>, profile: &Profile) -> Result<()> {
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    write_cas(
        &ctx.config.join(PROFILE),
        before,
        &serde_json::to_vec(profile)?,
    )
}

fn line(prompt: &str) -> Result<String> {
    eprint!("{prompt} ");
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(value.trim().into())
}

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        eprintln!();
    }
}

fn password() -> Result<String> {
    terminal::enable_raw_mode()?;
    let _restore = Restore;
    eprint!("API key（输入不回显，Esc 取消） ");
    let mut value = String::new();
    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        match key.code {
            KeyCode::Esc => return Ok(String::new()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(String::new());
            }
            KeyCode::Enter => return Ok(value),
            KeyCode::Backspace => {
                value.pop();
            }
            KeyCode::Char(ch) if ch.is_ascii() && value.len() < 513 => value.push(ch),
            _ => (),
        }
    }
}

pub(crate) fn run(ctx: &Context, json: bool, dry_run: bool) -> Result<Value> {
    let before = regular(&ctx.config.join(PROFILE))?;
    let current = decode(before.as_deref())?;
    if json || dry_run {
        return Ok(status(current.as_ref()));
    }
    if !crate::browser::interactive_terminal() {
        return Err(Error::attention(
            "model_setup_terminal_required",
            "在终端运行 mo setup，或加 --json 查看状态",
        ));
    }
    let keys = NativeKeychain((!ctx.isolated).then_some(ctx.config.as_path()));
    wizard(
        ctx,
        before.as_deref(),
        current,
        &keys,
        line,
        password,
        |profile, token| {
            if ctx.isolated || !cfg!(target_os = "macos") {
                return Err(keychain_error());
            }
            crate::recommend_model::test_profile(ctx, profile, token)
        },
    )
}

fn wizard(
    ctx: &Context,
    before: Option<&[u8]>,
    current: Option<Profile>,
    keys: &impl KeyStore,
    mut ask: impl FnMut(&str) -> Result<String>,
    read_key: impl FnOnce() -> Result<String>,
    test_connection: impl FnOnce(&Profile, &str) -> Result<()>,
) -> Result<Value> {
    eprintln!("推荐模型\n  1 DeepSeek\n  2 自定义 OpenAI 兼容服务\n  3 本地分析\n  Enter 取消");
    let next = match ask("选择")?.as_str() {
        "1" => profile("https://api.deepseek.com", recommend_model::MODEL)?,
        "2" => profile(&ask("HTTPS Base URL")?, &ask("模型名")?)?,
        "3" => {
            if let Some(mut p) = current {
                p.enabled = false;
                disable_profile(ctx, before, &p)?;
                return Ok(status(Some(&p)));
            }
            return Ok(status(None));
        }
        "" => return Ok(json!({"mode":"cancelled"})),
        _ => return Err(invalid()),
    };
    let token = read_key()?;
    if token.is_empty() {
        return Ok(json!({"mode":"cancelled"}));
    }
    let token = recommend_model::api_token(token.as_bytes())?;
    eprintln!("测试地址 {} · 模型 {}", next.endpoint, next.model);
    if ask("发送最小测试请求？[y/N]")? != "y" {
        return Ok(json!({"mode":"cancelled"}));
    }
    test_connection(&next, token)?;
    if ask("连接成功。保存设置？[y/N]")? != "y" {
        return Ok(json!({"mode":"cancelled"}));
    }
    publish(ctx, before, &next, Some(token), keys)?;
    Ok(status(Some(&next)))
}

#[cfg(test)]
#[path = "../tests/unit/recommend_setup.rs"]
mod tests;
