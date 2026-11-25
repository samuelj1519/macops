use super::{
    keychain::{self, BorrowedItem, EntryState, ExternalItem, ReadFailure, Secret},
    model, model_policy as recommend_model,
};
use crate::{
    capability::scoped_files::{private, regular, write_cas_checked},
    context::RecommendSetupContext,
    error::{Error, Result},
    platform::fs::Directory,
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
    time::{Duration, Instant},
};

const PROFILE: &str = "recommend-model.json";
const PENDING: &str = "recommend-model-pending.json";

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Profile {
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
    Error::attention(
        "model_keychain_unavailable",
        "电脑上的钥匙串没能完成这一步。",
    )
}

fn read_error(error: ReadFailure) -> Error {
    match error {
        ReadFailure::Deadline => Error::attention("model_keychain_timeout", "等钥匙串回应太久了。"),
        ReadFailure::Keychain => keychain_error(),
        ReadFailure::Missing => Error::attention(
            "model_keychain_item_missing",
            "钥匙串找不到已保存的密钥。请重新设置或绑定密钥。",
        ),
        ReadFailure::Authorization => Error::attention(
            "model_keychain_authorization_required",
            "请解锁钥匙串，并在 macOS 提示中允许访问此密钥。",
        ),
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
        schema_version: 2,
        endpoint,
        model: model.into(),
        account: format!("setup-{:x}", Sha256::digest(nonce)),
        enabled: true,
    })
}

impl Profile {
    pub(crate) fn service(&self, config: &Path) -> String {
        if self.uses_single_entry() {
            return "org.macops.recommend.api-key".into();
        }
        let mut binding = Sha256::new();
        binding.update(config.as_os_str().as_bytes());
        binding.update([0]);
        binding.update(self.endpoint.as_bytes());
        format!("org.macops.recommend.endpoint.{:x}", binding.finalize())
    }

    pub(crate) fn account(&self) -> &str {
        if self.uses_single_entry() {
            "default"
        } else {
            &self.account
        }
    }

    pub(super) fn uses_single_entry(&self) -> bool {
        self.schema_version == 2
    }

    pub(super) fn credential_version(&self) -> &str {
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
    if !matches!(p.schema_version, 1 | 2)
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

pub(super) fn read(config: &Path) -> Result<Option<Profile>> {
    decode(regular(&config.join(PROFILE))?.as_deref())
}

trait KeyStore {
    fn add(
        &self,
        profile: &Profile,
        source: Secret<'_>,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<()>;
    fn write_single(
        &self,
        previous: Option<&Profile>,
        next: &Profile,
        source: Secret<'_>,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<()>;
    fn entry_state(&self, previous: Option<&Profile>, next: &Profile) -> Result<EntryState>;
    fn remove(
        &self,
        profile: &Profile,
        require_tag: bool,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<()>;
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
    fn add(
        &self,
        profile: &Profile,
        source: Secret<'_>,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        keychain::add(self.config()?, profile, source, guard)
    }

    fn write_single(
        &self,
        previous: Option<&Profile>,
        next: &Profile,
        source: Secret<'_>,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        keychain::write_single(self.config()?, previous, next, source, guard)
    }

    fn entry_state(&self, previous: Option<&Profile>, next: &Profile) -> Result<EntryState> {
        keychain::entry_state(self.config()?, previous, next)
    }

    fn remove(
        &self,
        profile: &Profile,
        require_tag: bool,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        keychain::remove(self.config()?, profile, require_tag, guard)
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
    a.schema_version == b.schema_version && a.account == b.account && a.endpoint == b.endpoint
}

#[derive(Debug)]
struct ProfileLock {
    lease: Directory,
    config: Directory,
}

impl ProfileLock {
    fn verify(&self) -> Result<()> {
        for directory in [&self.lease, &self.config] {
            directory.verify()?;
            let metadata = directory.file().metadata()?;
            if metadata.uid() != nix::unistd::geteuid().as_raw()
                || metadata.mode() & 0o7777 != 0o700
            {
                return Err(Error::attention(
                    "model_setup_lock_changed",
                    "设置锁或配置目录的权限已改变",
                ));
            }
        }
        Ok(())
    }

    fn check_record(&self, name: &str, expected: Option<&[u8]>) -> Result<()> {
        if regular(&self.config.path.join(name))?.as_deref() != expected {
            return Err(single_pending());
        }
        Ok(())
    }

    fn verify_records(&self, profile: Option<&[u8]>, pending: Option<&[u8]>) -> Result<()> {
        self.verify()?;
        self.check_record(PROFILE, profile)?;
        self.check_record(PENDING, pending)?;
        self.verify()
    }

    fn empty_intent(&self) -> Result<Option<Vec<u8>>> {
        let raw = regular(&self.config.path.join(PENDING))?;
        if optional_record::<Pending>(raw.as_deref())?.is_some() {
            return Err(single_pending());
        }
        Ok(raw)
    }

    fn write_profile(
        &self,
        before: Option<&[u8]>,
        after: &[u8],
        pending: Option<&[u8]>,
    ) -> Result<()> {
        write_cas_checked(&self.config.path.join(PROFILE), before, after, || {
            self.verify_records(before, pending)
        })
    }

    fn write(&self, name: &str, before: Option<&[u8]>, after: &[u8]) -> Result<()> {
        write_cas_checked(&self.config.path.join(name), before, after, || {
            self.verify()
        })
    }
}

fn lock_path(ctx: &RecommendSetupContext) -> Result<std::path::PathBuf> {
    let home = if ctx.isolated {
        ctx.home.clone()
    } else {
        nix::unistd::User::from_uid(nix::unistd::geteuid())
            .map_err(|_| keychain_error())?
            .ok_or_else(keychain_error)?
            .dir
    };
    Ok(home.join(".config/macops/recommend-model.lock"))
}

fn lock_profile(ctx: &RecommendSetupContext, before: Option<&[u8]>) -> Result<ProfileLock> {
    let lease = private(&lock_path(ctx)?)?;
    lease.file().try_lock_exclusive().map_err(|error| {
        if error.kind() == io::ErrorKind::WouldBlock {
            Error::attention("model_setup_busy", "另一进程正在保存或核对推荐模型设置")
        } else {
            error.into()
        }
    })?;
    if regular(&ctx.config.join(PROFILE))?.as_deref() != before {
        return Err(Error::attention(
            "model_setup_stale",
            "模型设置已改变，请重新打开设置",
        ));
    }
    let lock = ProfileLock {
        lease,
        config: private(&ctx.config)?,
    };
    lock.verify()?;
    Ok(lock)
}

fn recover_pending(
    lock: &ProfileLock,
    current: Option<&Profile>,
    keys: &impl KeyStore,
) -> Result<()> {
    let raw = regular(&lock.config.path.join(PENDING))?;
    let guard = || {
        lock.verify()?;
        if read(&lock.config.path)?.as_ref() != current {
            return Err(single_pending());
        }
        lock.check_record(PENDING, raw.as_deref())?;
        lock.verify()
    };
    if let Some(staged) = optional_record::<Pending>(raw.as_deref())? {
        validate(&staged.profile)?;
        if staged.profile.uses_single_entry()
            || staged
                .previous
                .as_ref()
                .is_some_and(Profile::uses_single_entry)
        {
            return Err(single_pending());
        }
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
                    keys.remove(previous, true, &guard)?;
                }
            } else {
                keys.remove(&staged.profile, staged.tagged, &guard)?;
            }
        }
        lock.write(PENDING, raw.as_deref(), b"null")?;
    }
    Ok(())
}

fn single_pending() -> Error {
    Error::attention(
        "model_keychain_update_pending",
        "上一次保存密钥的结果还没确认。运行 mo recommend --setup，选择 c 检查那次的结果。",
    )
}

fn retire_legacy(
    lock: &ProfileLock,
    previous: Option<&Profile>,
    next: &Profile,
    pending: &[u8],
    keys: &impl KeyStore,
) -> Result<()> {
    if let Some(profile) = previous.filter(|p| !p.uses_single_entry()) {
        let cleanup_pending = |_| {
            Error::attention(
                "model_keychain_cleanup_pending",
                "新凭据已保存。选择 c 继续核对旧条目。",
            )
        };
        let verified = keys.load(next, deadline(15)).map_err(cleanup_pending)?;
        recommend_model::api_token(&verified).map_err(cleanup_pending)?;
        let guard = || {
            lock.verify()?;
            if read(&lock.config.path)?.as_ref() != Some(next) {
                return Err(single_pending());
            }
            lock.check_record(PENDING, Some(pending))?;
            lock.verify()
        };
        guard()?;
        keys.remove(profile, true, &guard)
            .map_err(cleanup_pending)?;
    }
    Ok(())
}

fn clear_single_record(
    lock: &ProfileLock,
    profile: Option<&[u8]>,
    pending: Option<&[u8]>,
) -> Result<()> {
    write_cas_checked(&lock.config.path.join(PENDING), pending, b"null", || {
        lock.verify_records(profile, pending)
    })
}

fn recover_single(
    lock: &ProfileLock,
    before: Option<&[u8]>,
    current: Option<Profile>,
    raw: &[u8],
    staged: Pending,
    keys: &impl KeyStore,
) -> Result<Option<Profile>> {
    validate(&staged.profile)?;
    if let Some(previous) = &staged.previous {
        validate(previous)?;
        if same_entry(previous, &staged.profile) {
            return Err(invalid());
        }
    }
    if staged.added
        || !staged.tagged
        || (current != staged.previous && current.as_ref() != Some(&staged.profile))
    {
        return Err(single_pending());
    }
    let previous = staged.previous.as_ref();
    let state = keys
        .entry_state(previous, &staged.profile)
        .map_err(|_| single_pending())?;
    let mut published = None;
    let active = match state {
        EntryState::Next => {
            if current == staged.previous {
                let latest = serde_json::to_vec(&staged.profile)?;
                lock.write_profile(before, &latest, Some(raw))?;
                published = Some(latest);
            }
            retire_legacy(lock, previous, &staged.profile, raw, keys)?;
            Some(staged.profile)
        }
        EntryState::Previous | EntryState::Missing if current == staged.previous => current,
        _ => return Err(single_pending()),
    };
    clear_single_record(lock, published.as_deref().or(before), Some(raw))?;
    Ok(active)
}

fn publish_single(
    lock: &ProfileLock,
    before: Option<&[u8]>,
    previous: Option<Profile>,
    next: &Profile,
    source: Secret<'_>,
    keys: &impl KeyStore,
) -> Result<()> {
    if previous.as_ref().is_some_and(|old| same_entry(old, next)) {
        return Err(invalid());
    }
    let raw = lock.empty_intent()?;
    if !previous.as_ref().is_some_and(Profile::uses_single_entry) && keys.contains(next)? {
        return Err(Error::attention(
            "model_keychain_slot_conflict",
            "固定钥匙串条目已存在，当前设置无法证明归属。",
        ));
    }
    let staged = Pending {
        profile: next.clone(),
        previous,
        added: false,
        tagged: true,
    };
    let intent = serde_json::to_vec(&staged)?;
    lock.write(PENDING, raw.as_deref(), &intent)?;
    let guard = || lock.verify_records(before, Some(&intent));
    keys.write_single(staged.previous.as_ref(), next, source, &guard)
        .map_err(|_| single_pending())?;
    if keys
        .entry_state(staged.previous.as_ref(), next)
        .map_err(|_| single_pending())?
        != EntryState::Next
    {
        return Err(single_pending());
    }
    let expected = serde_json::to_vec(next)?;
    lock.write_profile(before, &expected, Some(&intent))?;
    retire_legacy(lock, staged.previous.as_ref(), next, &intent, keys)?;
    clear_single_record(lock, Some(&expected), Some(&intent))
}

fn publish(
    ctx: &RecommendSetupContext,
    before: Option<&[u8]>,
    new: &Profile,
    source: Option<Secret<'_>>,
    keys: &impl KeyStore,
) -> Result<()> {
    let lock = lock_profile(ctx, before)?;
    let previous = decode(before)?;
    recover_pending(&lock, previous.as_ref(), keys)?;
    if let Some(source) = source
        && new.uses_single_entry()
    {
        return publish_single(&lock, before, previous, new, source, keys);
    }
    let encoded = serde_json::to_vec(new)?;
    let mut pending = lock.empty_intent()?;
    let replacing = source.is_some();
    if let Some(source) = source {
        if previous.as_ref().is_some_and(|old| same_entry(old, new)) {
            return Err(invalid());
        }
        let mut staged = Pending {
            profile: new.clone(),
            added: false,
            previous,
            tagged: true,
        };
        let intent = serde_json::to_vec(&staged)?;
        lock.write(PENDING, pending.as_deref(), &intent)?;
        keys.add(new, source, &|| lock.verify_records(before, Some(&intent)))?;
        staged.added = true;
        let written = serde_json::to_vec(&staged)?;
        lock.write(PENDING, Some(&intent), &written)?;
        pending = Some(written);
    }
    lock.write_profile(before, &encoded, pending.as_deref())?;
    if replacing {
        recover_pending(&lock, Some(new), keys).map_err(|_| {
            Error::attention(
                "model_keychain_cleanup_pending",
                "设置已保存，钥匙串清理待重试",
            )
        })?;
    }
    Ok(())
}

fn recover_setup(
    ctx: &RecommendSetupContext,
    before: Option<&[u8]>,
    keys: &impl KeyStore,
) -> Result<Value> {
    let current = decode(before)?;
    let pending_path = ctx.config.join(PENDING);
    if optional_record::<Pending>(regular(&pending_path)?.as_deref())?.is_none() {
        return Ok(status(current.as_ref()));
    }
    let lock = lock_profile(ctx, before)?;
    if let Some(raw) = regular(&lock.config.path.join(PENDING))?
        && let Some(staged) = optional_record::<Pending>(Some(&raw))?
        && staged.profile.uses_single_entry()
    {
        return recover_single(&lock, before, current, &raw, staged, keys)
            .map(|profile| status(profile.as_ref()));
    }
    recover_pending(&lock, current.as_ref(), keys)?;
    Ok(status(current.as_ref()))
}

fn retainable(current: Option<&Profile>, raw: Option<&[u8]>) -> Result<Pending> {
    let unavailable = || {
        Error::attention(
            "model_keychain_cleanup_not_committed",
            "仅能结束已保存新配置的旧条目清理",
        )
    };
    let staged = optional_record::<Pending>(raw)?.ok_or_else(unavailable)?;
    let previous = staged.previous.as_ref().ok_or_else(unavailable)?;
    validate(previous)?;
    validate(&staged.profile)?;
    let committed = if staged.profile.uses_single_entry() {
        !staged.added && current == Some(&staged.profile)
    } else {
        staged.added && current.is_some_and(|p| same_entry(p, &staged.profile))
    };
    if !staged.tagged
        || !committed
        || previous.uses_single_entry()
        || same_entry(previous, &staged.profile)
    {
        return Err(unavailable());
    }
    Ok(staged)
}

fn retain_setup(
    ctx: &RecommendSetupContext,
    before: Option<&[u8]>,
    pending: Option<&[u8]>,
    keys: &impl KeyStore,
) -> Result<Value> {
    let lock = lock_profile(ctx, before)?;
    let current = decode(before)?;
    let staged = retainable(current.as_ref(), pending)?;
    if staged.profile.uses_single_entry()
        && keys.entry_state(staged.previous.as_ref(), &staged.profile)? != EntryState::Next
    {
        return Err(single_pending());
    }
    clear_single_record(&lock, before, pending)?;
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

fn disable_profile(
    ctx: &RecommendSetupContext,
    before: Option<&[u8]>,
    profile: &Profile,
) -> Result<()> {
    let lock = lock_profile(ctx, before)?;
    let pending = regular(&lock.config.path.join(PENDING))?;
    reject_single_pending(ctx)?;
    lock.write_profile(before, &serde_json::to_vec(profile)?, pending.as_deref())
}

fn reject_single_pending(ctx: &RecommendSetupContext) -> Result<()> {
    let raw = regular(&ctx.config.join(PENDING))?;
    if optional_record::<Pending>(raw.as_deref())?.is_some_and(|op| op.profile.uses_single_entry())
    {
        return Err(single_pending());
    }
    Ok(())
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

pub(crate) fn run(ctx: &RecommendSetupContext, json: bool, dry_run: bool) -> Result<Value> {
    let before = regular(&ctx.config.join(PROFILE))?;
    let current = decode(before.as_deref())?;
    if json || dry_run {
        return Ok(status(current.as_ref()));
    }
    if !crate::browser::interactive_terminal() {
        return Err(Error::attention(
            "model_setup_terminal_required",
            "在终端运行 mo recommend --setup，或加 --json 查看状态",
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
            model::test_profile(profile, token, until)
        },
    )
}

fn wizard(
    ctx: &RecommendSetupContext,
    before: Option<&[u8]>,
    current: Option<Profile>,
    keys: &impl KeyStore,
    mut ask: impl FnMut(&str) -> Result<String>,
    read_key: impl FnOnce() -> Result<String>,
    test_connection: impl FnOnce(&Profile, &str, Instant) -> Result<()>,
) -> Result<Value> {
    eprintln!("推荐模型");
    if let Some(profile) = &current {
        eprintln!(
            "当前模式 {} · {} · {}",
            if profile.enabled { "远端" } else { "本地" },
            profile.endpoint,
            profile.model,
        );
    } else {
        eprintln!("当前模式 本地分析（尚未配置远端）");
    }
    eprintln!(
        "  1 DeepSeek（设置或换钥）\n  2 自定义 OpenAI 兼容服务（设置或换钥）\n  3 使用本地分析"
    );
    if let Some(profile) = &current {
        eprintln!("  t 测试已保存密钥  m 更改模型");
        if !profile.enabled {
            eprintln!("  r 重新启用已保存的模型");
        }
    }
    eprintln!(
        "钥匙串维护\n  b 绑定已有钥匙串条目  c 核对待完成的钥匙串操作\n  k 保留旧条目并结束待处理清理\n  Enter 取消"
    );
    let action = ask("选择")?;
    if matches!(action.as_str(), "1" | "2" | "3" | "b" | "r" | "m") {
        reject_single_pending(ctx)?;
    }
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
            return retain_setup(ctx, before, pending.as_deref(), keys);
        }
        "" => return Ok(cancelled()),
        _ => return Err(invalid()),
    };
    if action == "b" {
        let path = ask("已有钥匙串文件的绝对路径")?;
        let item =
            ExternalItem::bind(Path::new(&path), ask("钥匙串服务名")?, ask("钥匙串账户名")?)?;
        if !confirm_test(ctx, &next, &mut ask, true)? {
            return Ok(cancelled());
        }
        let until = deadline(ctx.timeout_secs);
        let bytes = keys.external(&item, until)?;
        reject_single_pending(ctx)?;
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
    if !confirm_test(ctx, &next, &mut ask, true)? {
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

fn confirm_test(
    ctx: &RecommendSetupContext,
    profile: &Profile,
    ask: &mut impl FnMut(&str) -> Result<String>,
    saving: bool,
) -> Result<bool> {
    eprintln!("测试地址 {} · 模型 {}", profile.endpoint, profile.model);
    if ask("发送最小测试请求？[y/N]")? != "y" {
        return Ok(false);
    }
    if saving {
        reject_single_pending(ctx)?;
    }
    Ok(true)
}

fn save_after_test(
    ctx: &RecommendSetupContext,
    before: Option<&[u8]>,
    profile: &Profile,
    source: Option<Secret<'_>>,
    keys: &impl KeyStore,
    ask: &mut impl FnMut(&str) -> Result<String>,
) -> Result<Value> {
    reject_single_pending(ctx)?;
    if ask("连接成功。保存设置？[y/N]")? != "y" {
        return Ok(cancelled());
    }
    publish(ctx, before, profile, source, keys)?;
    Ok(status(Some(profile)))
}

fn reuse(
    ctx: &RecommendSetupContext,
    before: Option<&[u8]>,
    mut next: Profile,
    action: &str,
    keys: &impl KeyStore,
    mut ask: impl FnMut(&str) -> Result<String>,
    test_connection: impl FnOnce(&Profile, &str, Instant) -> Result<()>,
) -> Result<Value> {
    if !confirm_test(ctx, &next, &mut ask, action != "t")? {
        return Ok(cancelled());
    }
    let until = deadline(ctx.timeout_secs);
    let bytes = keys.load(&next, until)?;
    if action != "t" {
        reject_single_pending(ctx)?;
    }
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
#[path = "../../tests/unit/recommend_setup.rs"]
mod tests;
