use crate::{
    context::Context,
    error::{Error, Result},
    native_skills_files::{regular, write_cas},
    policy::recommend_model,
    recommend_keychain::{self as keychain, BorrowedItem, ExternalItem, ReadFailure, Secret},
    store::Store,
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::Path,
    time::{Duration, Instant},
};

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

fn read_error(error: ReadFailure) -> Error {
    match error {
        ReadFailure::Deadline => Error::attention("model_keychain_timeout", "钥匙串读取超时"),
        ReadFailure::Keychain => keychain_error(),
    }
}

fn deadline(seconds: u64) -> Instant {
    Instant::now() + Duration::from_secs(seconds.min(15))
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
    fn add(&self, profile: &Profile, source: Secret<'_>) -> Result<()>;
    fn remove(&self, profile: &Profile, require_tag: bool) -> Result<()>;
    fn contains(&self, profile: &Profile) -> Result<bool>;
    fn load(&self, profile: &Profile, deadline: Instant) -> Result<Vec<u8>>;
    fn external(&self, item: &ExternalItem, deadline: Instant) -> Result<Vec<u8>>;
}

struct NativeKeychain<'a>(Option<&'a Path>);
impl<'a> NativeKeychain<'a> {
    fn config(&self) -> Result<&'a Path> {
        self.0.ok_or_else(keychain_error)
    }
}

impl KeyStore for NativeKeychain<'_> {
    fn add(&self, profile: &Profile, source: Secret<'_>) -> Result<()> {
        keychain::add(self.config()?, profile, source)
    }

    fn remove(&self, profile: &Profile, require_tag: bool) -> Result<()> {
        keychain::remove(self.config()?, profile, require_tag)
    }

    fn contains(&self, profile: &Profile) -> Result<bool> {
        keychain::contains(self.config()?, profile)
    }

    fn load(&self, profile: &Profile, deadline: Instant) -> Result<Vec<u8>> {
        keychain::load_until(profile, self.config()?, deadline).map_err(read_error)
    }

    fn external(&self, item: &ExternalItem, deadline: Instant) -> Result<Vec<u8>> {
        self.config()?;
        let item = item.clone();
        keychain::bounded(deadline, move || keychain::external(&item)).map_err(read_error)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    profile: Profile,
    added: bool,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    previous: Option<Profile>,
    #[serde(default)]
    tagged: bool,
}

fn same_entry(a: &Profile, b: &Profile) -> bool {
    a.account() == b.account() && a.endpoint == b.endpoint
}

fn recover_pending(dir: &Path, current: Option<&Profile>, keys: &impl KeyStore) -> Result<()> {
    let path = dir.join(PENDING);
    let raw = regular(&path)?;
    if let Some(staged) = optional_record::<Pending>(raw.as_deref())? {
        validate(&staged.profile)?;
        if let Some(previous) = &staged.previous {
            validate(previous)?;
            if same_entry(previous, &staged.profile) {
                return Err(invalid());
            }
        }
        if !staged.added && keys.contains(&staged.profile)? {
            return Err(Error::attention(
                "model_keychain_confirmation_pending",
                "原钥匙串写入待核对，已保留原配置和待处理记录",
            ));
        }
        if staged.added {
            if current.is_some_and(|profile| same_entry(profile, &staged.profile)) {
                if let Some(previous) = &staged.previous {
                    let verified = keys
                        .load(&staged.profile, deadline(15))
                        .and_then(|bytes| recommend_model::api_token(&bytes).map(|_| ()));
                    if verified.is_err() {
                        return Err(Error::attention(
                            "model_keychain_cleanup_pending",
                            "新钥匙串凭据暂不可用，原条目已保留",
                        ));
                    }
                    keys.remove(previous, true)?;
                }
            } else {
                keys.remove(&staged.profile, staged.tagged)?;
            }
        }
        write_cas(&path, raw.as_deref(), b"null")?;
    }
    Ok(())
}

fn lock_profile(ctx: &Context, before: Option<&[u8]>) -> Result<Store> {
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    if regular(&ctx.config.join(PROFILE))?.as_deref() != before {
        return Err(Error::attention(
            "model_setup_stale",
            "模型设置已改变，请重新打开设置",
        ));
    }
    Ok(store)
}

fn publish(
    ctx: &Context,
    before: Option<&[u8]>,
    new: &Profile,
    source: Option<Secret<'_>>,
    keys: &impl KeyStore,
) -> Result<()> {
    let _store = lock_profile(ctx, before)?;
    let target = ctx.config.join(PROFILE);
    let previous = decode(before)?;
    recover_pending(&ctx.config, previous.as_ref(), keys)?;
    let encoded = serde_json::to_vec(new)?;
    let pending = ctx.config.join(PENDING);
    let replacing = source.is_some();
    if let Some(source) = source {
        if previous.as_ref().is_some_and(|old| same_entry(old, new)) {
            return Err(invalid());
        }
        let raw = regular(&pending)?;
        let mut staged = Pending {
            profile: new.clone(),
            added: false,
            previous,
            tagged: true,
        };
        let intent = serde_json::to_vec(&staged)?;
        write_cas(&pending, raw.as_deref(), &intent)?;
        keys.add(new, source)?;
        staged.added = true;
        let written = serde_json::to_vec(&staged)?;
        write_cas(&pending, Some(&intent), &written)?;
    }
    write_cas(&target, before, &encoded)?;
    if replacing {
        recover_pending(&ctx.config, Some(new), keys).map_err(|_| {
            Error::attention(
                "model_keychain_cleanup_pending",
                "设置已保存，钥匙串清理待重试",
            )
        })?;
    }
    Ok(())
}

fn recover_setup(ctx: &Context, before: Option<&[u8]>, keys: &impl KeyStore) -> Result<Value> {
    let _store = lock_profile(ctx, before)?;
    let current = decode(before)?;
    recover_pending(&ctx.config, current.as_ref(), keys)?;
    Ok(status(current.as_ref()))
}

fn retainable(current: Option<&Profile>, raw: Option<&[u8]>) -> Result<()> {
    let unavailable = || {
        Error::attention(
            "model_keychain_cleanup_not_committed",
            "仅能结束已保存新配置的旧条目清理",
        )
    };
    let staged = optional_record::<Pending>(raw)?
        .filter(|entry| {
            entry.added && entry.tagged && current.is_some_and(|p| same_entry(p, &entry.profile))
        })
        .ok_or_else(unavailable)?;
    let previous = staged.previous.ok_or_else(unavailable)?;
    validate(&previous)?;
    validate(&staged.profile)?;
    if same_entry(&previous, &staged.profile) {
        return Err(unavailable());
    }
    Ok(())
}

fn retain_setup(ctx: &Context, before: Option<&[u8]>, pending: Option<&[u8]>) -> Result<Value> {
    let _store = lock_profile(ctx, before)?;
    let current = decode(before)?;
    retainable(current.as_ref(), pending)?;
    write_cas(&ctx.config.join(PENDING), pending, b"null")?;
    let mut value = status(current.as_ref());
    value["cleanup"] = json!("previous-entry-retained");
    Ok(value)
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
    let _hold = crate::terminal::SignalHold::enter()?;
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
        |profile, token, until| {
            if ctx.isolated || !cfg!(target_os = "macos") {
                return Err(keychain_error());
            }
            crate::recommend_model::test_profile(profile, token, until)
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
    test_connection: impl FnOnce(&Profile, &str, Instant) -> Result<()>,
) -> Result<Value> {
    if let Some(profile) = &current {
        eprintln!(
            "当前模式 {} · {} · {}",
            if profile.enabled { "远端" } else { "本地" },
            profile.endpoint,
            profile.model,
        );
    }
    eprintln!(
        "推荐模型\n  1 DeepSeek（新建或换钥）\n  2 自定义 OpenAI 兼容服务（新建或换钥）\n  3 本地分析\n  t 测试已保存密钥  r 重新启用  m 更改模型\n  b 绑定已有钥匙串条目  c 核对待完成的钥匙串操作\n  k 保留旧条目并结束待处理清理\n  Enter 取消"
    );
    let action = ask("选择")?;
    let next = match action.as_str() {
        "1" => profile("https://api.deepseek.com", recommend_model::MODEL)?,
        "2" | "b" => profile(&ask("HTTPS Base URL")?, &ask("模型名")?)?,
        "3" => {
            if let Some(mut p) = current {
                p.enabled = false;
                disable_profile(ctx, before, &p)?;
                return Ok(status(Some(&p)));
            }
            return Ok(status(None));
        }
        "t" | "r" | "m" => {
            let mut next = current.ok_or_else(|| {
                Error::attention("model_profile_missing", "请先保存推荐模型和钥匙串凭据")
            })?;
            if action == "m" {
                next.model = ask("新的模型名（沿用现有服务地址）")?;
                validate(&next)?;
            }
            return reuse(ctx, before, next, &action, keys, ask, test_connection);
        }
        "c" => return recover_setup(ctx, before, keys),
        "k" => {
            let pending = regular(&ctx.config.join(PENDING))?;
            retainable(current.as_ref(), pending.as_deref())?;
            if ask("保留旧条目并结束清理，当前配置仍会生效。确认？[y/N]")? != "y"
            {
                return Ok(cancelled());
            }
            return retain_setup(ctx, before, pending.as_deref());
        }
        "" => return Ok(cancelled()),
        _ => return Err(invalid()),
    };
    if action == "b" {
        let path = ask("已有钥匙串文件的绝对路径")?;
        let item =
            ExternalItem::bind(Path::new(&path), ask("钥匙串服务名")?, ask("钥匙串账户名")?)?;
        if !confirm_test(&next, &mut ask)? {
            return Ok(cancelled());
        }
        let until = deadline(ctx.timeout_secs);
        let bytes = keys.external(&item, until)?;
        let token = recommend_model::api_token(&bytes)?;
        test_before(&next, token, until, test_connection)?;
        let bound = BorrowedItem::verified(item, token);
        return save_after_test(
            ctx,
            before,
            &next,
            Some(Secret::Existing(&bound)),
            keys,
            &mut ask,
        );
    }
    let token = read_key()?;
    if token.is_empty() {
        return Ok(cancelled());
    }
    let token = recommend_model::api_token(token.as_bytes())?;
    if !confirm_test(&next, &mut ask)? {
        return Ok(cancelled());
    }
    test_before(&next, token, deadline(ctx.timeout_secs), test_connection)?;
    save_after_test(ctx, before, &next, Some(Secret::New(token)), keys, &mut ask)
}

fn test_before(
    profile: &Profile,
    token: &str,
    until: Instant,
    send: impl FnOnce(&Profile, &str, Instant) -> Result<()>,
) -> Result<()> {
    if Instant::now() >= until {
        return Err(read_error(ReadFailure::Deadline));
    }
    send(profile, token, until)
}

fn cancelled() -> Value {
    json!({"mode":"cancelled"})
}

fn confirm_test(profile: &Profile, ask: &mut impl FnMut(&str) -> Result<String>) -> Result<bool> {
    eprintln!("测试地址 {} · 模型 {}", profile.endpoint, profile.model);
    Ok(ask("发送最小测试请求？[y/N]")? == "y")
}

fn save_after_test(
    ctx: &Context,
    before: Option<&[u8]>,
    profile: &Profile,
    source: Option<Secret<'_>>,
    keys: &impl KeyStore,
    ask: &mut impl FnMut(&str) -> Result<String>,
) -> Result<Value> {
    if ask("连接成功。保存设置？[y/N]")? != "y" {
        return Ok(cancelled());
    }
    publish(ctx, before, profile, source, keys)?;
    Ok(status(Some(profile)))
}

fn reuse(
    ctx: &Context,
    before: Option<&[u8]>,
    mut next: Profile,
    action: &str,
    keys: &impl KeyStore,
    mut ask: impl FnMut(&str) -> Result<String>,
    test_connection: impl FnOnce(&Profile, &str, Instant) -> Result<()>,
) -> Result<Value> {
    if !confirm_test(&next, &mut ask)? {
        return Ok(cancelled());
    }
    let until = deadline(ctx.timeout_secs);
    let bytes = keys.load(&next, until)?;
    test_before(
        &next,
        recommend_model::api_token(&bytes)?,
        until,
        test_connection,
    )?;
    if action == "t" {
        let mut value = status(Some(&next));
        value["connection"] = json!("verified");
        return Ok(value);
    }
    if action == "r" {
        next.enabled = true;
    }
    save_after_test(ctx, before, &next, None, keys, &mut ask)
}

#[cfg(test)]
#[path = "../tests/unit/recommend_setup.rs"]
mod tests;
