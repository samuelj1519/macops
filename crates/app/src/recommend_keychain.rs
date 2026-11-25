use crate::{
    domain::FileId,
    error::{Error, Result},
    paths,
    policy::{config_local::valid_digest, recommend_model::api_token},
    recommend_setup::Profile,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Instant,
};

const FORMAT: &[u8] = b"macops-credential-v1\0";
const OWNED: u8 = 1;
const BORROWED: u8 = 2;
const MAX_VALUE: usize = 2048;
const RESERVED_SERVICE: &str = "org.macops.recommend.";

pub(super) fn unavailable() -> Error {
    Error::attention("model_keychain_unavailable", "无法读取或更新指定钥匙串条目")
}

fn missing() -> Error {
    Error::attention(
        "model_keychain_item_missing",
        "钥匙串找不到推荐模型已保存的密钥",
    )
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExternalItem {
    path: PathBuf,
    file: FileId,
    service: String,
    account: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BorrowedItem {
    item: ExternalItem,
    fingerprint: String,
}

impl ExternalItem {
    pub(super) fn bind(path: &Path, service: String, account: String) -> Result<Self> {
        let path = paths::read_path(path)?;
        let item = Self {
            file: file_identity(&path)?,
            path,
            service,
            account,
        };
        item.validate()?;
        Ok(item)
    }

    fn validate(&self) -> Result<()> {
        let allowed = |value: &str| {
            !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        };
        if !allowed(&self.service)
            || !allowed(&self.account)
            || self
                .service
                .get(..RESERVED_SERVICE.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RESERVED_SERVICE))
            || !self.path.is_absolute()
            || self.path.as_os_str().len() > 1024
        {
            return Err(unavailable());
        }
        Ok(())
    }
}

fn file_identity(path: &Path) -> Result<FileId> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
    if !metadata.is_file() || paths::read_path(path)? != path {
        return Err(unavailable());
    }
    Ok(FileId {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

pub(super) fn external(item: &ExternalItem) -> Result<Vec<u8>> {
    external_from(item, native::read)
}

fn external_from(
    item: &ExternalItem,
    read: impl FnOnce(Option<&Path>, &str, &str) -> Result<Option<Vec<u8>>>,
) -> Result<Vec<u8>> {
    item.validate()?;
    if file_identity(&item.path)? != item.file {
        return Err(unavailable());
    }
    let bytes = read(Some(&item.path), &item.service, &item.account)?.ok_or_else(missing)?;
    if file_identity(&item.path)? != item.file {
        return Err(unavailable());
    }
    api_token(&bytes)?;
    Ok(bytes)
}

fn digest(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

impl BorrowedItem {
    pub(super) fn verified(item: ExternalItem, token: &str) -> Self {
        Self {
            item,
            fingerprint: digest(token.as_bytes()),
        }
    }

    fn resolve(&self, read: impl FnOnce(&ExternalItem) -> Result<Vec<u8>>) -> Result<Vec<u8>> {
        let bytes = read(&self.item)?;
        if digest(api_token(&bytes)?.as_bytes()) != self.fingerprint {
            return Err(Error::attention(
                "model_keychain_binding_changed",
                "外部钥匙串密钥已改变，请重新绑定",
            ));
        }
        Ok(bytes)
    }
}

#[derive(Clone, Copy)]
pub(super) enum Secret<'a> {
    New(&'a str),
    Existing(&'a BorrowedItem),
}

fn stamp(profile: &Profile, config: &Path) -> String {
    let mut hash = Sha256::new();
    hash.update(profile.service(config));
    hash.update([0]);
    hash.update(profile.account());
    if profile.uses_single_entry() {
        use std::os::unix::ffi::OsStrExt;
        hash.update([0]);
        hash.update(config.as_os_str().as_bytes());
        hash.update([0]);
        hash.update(profile.endpoint.as_bytes());
        hash.update([0]);
        hash.update(profile.credential_version());
    }
    format!("{:x}", hash.finalize())
}

fn encode(profile: &Profile, config: &Path, source: Secret<'_>) -> Result<Vec<u8>> {
    let mut buf = FORMAT.to_vec();
    buf.extend_from_slice(stamp(profile, config).as_bytes());
    match source {
        Secret::New(token) => {
            buf.push(OWNED);
            buf.extend_from_slice(token.as_bytes());
        }
        Secret::Existing(reference) => {
            buf.push(BORROWED);
            serde_json::to_writer(&mut buf, reference)?;
        }
    }
    if buf.len() > MAX_VALUE {
        return Err(unavailable());
    }
    Ok(buf)
}

enum Slot {
    Legacy,
    Owned,
    Borrowed(BorrowedItem),
}

fn slot(profile: &Profile, config: &Path, bytes: &[u8]) -> Result<Slot> {
    let Some(rest) = bytes.strip_prefix(FORMAT) else {
        if profile.uses_single_entry() {
            return Err(unavailable());
        }
        api_token(bytes)?;
        return Ok(Slot::Legacy);
    };
    let rest = rest
        .strip_prefix(stamp(profile, config).as_bytes())
        .ok_or_else(unavailable)?;
    match rest.split_first() {
        Some((&OWNED, token)) => {
            api_token(token)?;
            Ok(Slot::Owned)
        }
        Some((&BORROWED, locator)) => {
            let crate::policy::json::Object(reference) =
                serde_json::from_value::<crate::policy::json::Object<BorrowedItem>>(
                    crate::policy::json::parse(locator).map_err(|_| unavailable())?,
                )
                .map_err(|_| unavailable())?;
            reference.item.validate()?;
            if !valid_digest(&reference.fingerprint) {
                return Err(unavailable());
            }
            Ok(Slot::Borrowed(reference))
        }
        _ => Err(unavailable()),
    }
}

fn resolve(
    profile: &Profile,
    config: &Path,
    mut bytes: Vec<u8>,
    read: impl FnOnce(&ExternalItem) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    match slot(profile, config, &bytes)? {
        Slot::Legacy => Ok(bytes),
        Slot::Owned => {
            bytes.drain(..FORMAT.len() + 65);
            Ok(bytes)
        }
        Slot::Borrowed(reference) => reference.resolve(read),
    }
}

pub(super) fn load(profile: &Profile, config: &Path, deadline: Option<Instant>) -> Result<Vec<u8>> {
    load_from(profile, config, deadline, native::read)
}

fn load_from(
    profile: &Profile,
    config: &Path,
    deadline: Option<Instant>,
    read: impl Fn(Option<&Path>, &str, &str) -> Result<Option<Vec<u8>>>,
) -> Result<Vec<u8>> {
    let bytes = read(None, &profile.service(config), profile.account())?.ok_or_else(missing)?;
    resolve(profile, config, bytes, |item| {
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            return Err(unavailable());
        }
        external_from(item, read)
    })
}

pub(super) fn load_until(
    profile: &Profile,
    config: &Path,
    deadline: Instant,
) -> std::result::Result<Vec<u8>, ReadFailure> {
    let profile = profile.clone();
    let config = config.to_owned();
    bounded(deadline, move || load(&profile, &config, Some(deadline)))
}

pub(super) fn load_quiet_until(
    profile: &Profile,
    config: &Path,
    deadline: Instant,
) -> std::result::Result<Vec<u8>, ReadFailure> {
    let profile = profile.clone();
    let config = config.to_owned();
    bounded(deadline, move || {
        native::without_interaction(|| load(&profile, &config, Some(deadline)))
    })
}

#[derive(PartialEq, Eq)]
pub(super) enum EntryState {
    Missing,
    Previous,
    Next,
}

fn classify_entry(
    config: &Path,
    previous: Option<&Profile>,
    next: &Profile,
    bytes: Option<&[u8]>,
) -> Result<EntryState> {
    let Some(bytes) = bytes else {
        return Ok(EntryState::Missing);
    };
    if slot(next, config, bytes).is_ok() {
        return Ok(EntryState::Next);
    }
    if previous.is_some_and(|p| p.uses_single_entry() && slot(p, config, bytes).is_ok()) {
        return Ok(EntryState::Previous);
    }
    Err(unavailable())
}

pub(super) fn entry_state(
    config: &Path,
    previous: Option<&Profile>,
    next: &Profile,
) -> Result<EntryState> {
    let bytes = native::read(None, &next.service(config), next.account())?;
    classify_entry(config, previous, next, bytes.as_deref())
}

pub(super) fn write_single(
    config: &Path,
    previous: Option<&Profile>,
    next: &Profile,
    source: Secret<'_>,
    guard: &dyn Fn() -> Result<()>,
) -> Result<()> {
    let previous = previous.filter(|p| p.uses_single_entry());
    let data = encode(next, config, source)?;
    native::write_single(
        &next.service(config),
        next.account(),
        &data,
        previous.is_some(),
        guard,
        |bytes| {
            matches!(
                classify_entry(config, previous, next, Some(bytes)),
                Ok(EntryState::Previous)
            )
        },
    )
}

pub(super) fn load_legacy(service: &str, account: &str) -> Result<Vec<u8>> {
    native::without_interaction(|| native::read(None, service, account)?.ok_or_else(missing))
}

pub(super) fn add(
    config: &Path,
    profile: &Profile,
    source: Secret<'_>,
    guard: &dyn Fn() -> Result<()>,
) -> Result<()> {
    native::add(
        &profile.service(config),
        profile.account(),
        &encode(profile, config, source)?,
        guard,
    )
}

pub(super) fn contains(config: &Path, profile: &Profile) -> Result<bool> {
    native::contains(&profile.service(config), profile.account())
}

pub(super) fn remove(
    config: &Path,
    profile: &Profile,
    require_tag: bool,
    guard: &dyn Fn() -> Result<()>,
) -> Result<()> {
    native::remove(
        &profile.service(config),
        profile.account(),
        require_tag,
        |data| {
            let eligible = eligible_removal(config, profile, data, require_tag)?;
            if eligible {
                guard()?;
            }
            Ok(eligible)
        },
    )
}

fn eligible_removal(
    config: &Path,
    profile: &Profile,
    data: &[u8],
    require_tag: bool,
) -> Result<bool> {
    if require_tag {
        return Ok(!matches!(slot(profile, config, data)?, Slot::Legacy));
    }
    Ok(true)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReadFailure {
    Deadline,
    Keychain,
    Missing,
    Authorization,
}

pub(super) fn bounded(
    deadline: Instant,
    load: impl FnOnce() -> Result<Vec<u8>> + Send + 'static,
) -> std::result::Result<Vec<u8>, ReadFailure> {
    let wait = deadline
        .checked_duration_since(Instant::now())
        .ok_or(ReadFailure::Deadline)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("macops-keychain".into())
        .spawn(move || {
            if Instant::now() < deadline {
                let _ = sender.send(load());
            }
        })
        .map_err(|_| ReadFailure::Keychain)?;
    let result = receiver.recv_timeout(wait);
    if Instant::now() >= deadline || matches!(result, Err(mpsc::RecvTimeoutError::Timeout)) {
        return Err(ReadFailure::Deadline);
    }
    result
        .map_err(|_| ReadFailure::Keychain)?
        .map_err(|error| match error.code.as_str() {
            "model_keychain_item_missing" => ReadFailure::Missing,
            "model_keychain_authorization_required" => ReadFailure::Authorization,
            _ => ReadFailure::Keychain,
        })
}

fn verified_item<P: AsRef<[u8]>, T>(
    unique: impl Fn() -> Result<bool>,
    find: impl FnOnce() -> Result<(P, T)>,
) -> Result<Option<(P, T)>> {
    if !unique()? {
        return Ok(None);
    }
    let (data, item) = find()?;
    if data.as_ref().len() > MAX_VALUE || !unique()? {
        return Err(unavailable());
    }
    Ok(Some((data, item)))
}

fn write_verified<P: AsRef<[u8]>, T>(
    present: impl Fn() -> Result<bool>,
    find: impl FnOnce() -> Result<(P, T)>,
    eligible: Option<impl FnOnce(&[u8]) -> bool>,
    write: impl FnOnce(Option<T>) -> Result<()>,
) -> Result<()> {
    let target = if present()? {
        let eligible = eligible.ok_or_else(unavailable)?;
        let (old, item) = verified_item(&present, find)?.ok_or_else(unavailable)?;
        if !eligible(old.as_ref()) {
            return Err(unavailable());
        }
        Some(item)
    } else {
        None
    };
    write(target)?;
    present()?.then_some(()).ok_or_else(unavailable)
}

fn delete_verified<P: AsRef<[u8]>, T>(
    unique: impl Fn() -> Result<bool>,
    find: impl FnOnce() -> Result<(P, T)>,
    eligible: impl FnOnce(&[u8]) -> Result<bool>,
    delete: impl FnOnce(T),
) -> Result<()> {
    let Some((data, item)) = verified_item(&unique, find)? else {
        return Ok(());
    };
    if eligible(data.as_ref())? {
        delete(item);
        if unique()? {
            return Err(unavailable());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod native {
    use super::{Path, Result, delete_verified, unavailable, verified_item, write_verified};
    use security_framework::{
        item::{ItemClass, ItemSearchOptions, SearchResult},
        os::macos::{keychain::SecKeychain, passwords::find_generic_password},
    };

    fn read_error(error: security_framework::base::Error) -> crate::error::Error {
        // errSecInteractionNotAllowed
        if error.code() == -25308 {
            return crate::error::Error::attention(
                "model_keychain_authorization_required",
                "macOS 钥匙串需要授权或解锁",
            );
        }
        unavailable()
    }

    fn exists(keychain: Option<&SecKeychain>, service: &str, account: &str) -> Result<bool> {
        let mut options = ItemSearchOptions::new();
        if let Some(keychain) = keychain {
            options.keychains(std::slice::from_ref(keychain));
        }
        let results = options
            .class(ItemClass::generic_password())
            .service(service)
            .account(account)
            .load_attributes(true)
            .limit(2)
            .search();
        unique_search(results)
    }

    pub(super) fn unique_search(
        results: security_framework::base::Result<Vec<SearchResult>>,
    ) -> Result<bool> {
        match results {
            Err(error) if error.code() == -25300 => Ok(false),
            Ok(rows) => unique_match(rows.len()),
            Err(error) => Err(read_error(error)),
        }
    }

    pub(super) fn unique_match(count: usize) -> Result<bool> {
        if count > 1 {
            return Err(unavailable());
        }
        Ok(count == 1)
    }

    pub(super) fn read(
        path: Option<&Path>,
        service: &str,
        account: &str,
    ) -> Result<Option<Vec<u8>>> {
        let keychain = path
            .map(|path| SecKeychain::open(path).map_err(|_| unavailable()))
            .transpose()?;
        let found = verified_item(
            || exists(keychain.as_ref(), service, account),
            || {
                find_generic_password(
                    keychain.as_ref().map(std::slice::from_ref),
                    service,
                    account,
                )
                .map_err(read_error)
            },
        )?;
        Ok(found.map(|(password, _)| password.to_vec()))
    }

    pub(super) fn contains(service: &str, account: &str) -> Result<bool> {
        exists(None, service, account)
    }

    pub(super) fn without_interaction<T>(load: impl FnOnce() -> Result<T>) -> Result<T> {
        let allowed = SecKeychain::user_interaction_allowed().map_err(|_| unavailable())?;
        let _guard = allowed
            .then(SecKeychain::disable_user_interaction)
            .transpose()
            .map_err(|_| unavailable())?;
        load()
    }

    pub(super) fn write_single(
        service: &str,
        account: &str,
        password: &[u8],
        replace: bool,
        guard: &dyn Fn() -> Result<()>,
        eligible: impl FnOnce(&[u8]) -> bool,
    ) -> Result<()> {
        let keychain = SecKeychain::default().map_err(|_| unavailable())?;
        let local = Some(std::slice::from_ref(&keychain));
        let present = || {
            let global = exists(None, service, account)?;
            (global == exists(Some(&keychain), service, account)?)
                .then_some(global)
                .ok_or_else(unavailable)
        };
        write_verified(
            present,
            || find_generic_password(local, service, account).map_err(|_| unavailable()),
            replace.then_some(eligible),
            |item| {
                guard()?;
                if let Some(mut item) = item {
                    item.set_password(password).map_err(|_| unavailable())
                } else {
                    keychain
                        .add_generic_password(service, account, password)
                        .map_err(|_| unavailable())
                }
            },
        )
    }

    pub(super) fn add(
        service: &str,
        account: &str,
        bytes: &[u8],
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        let keychain = SecKeychain::default().map_err(|_| unavailable())?;
        guard()?;
        keychain
            .add_generic_password(service, account, bytes)
            .map_err(|_| unavailable())
    }

    pub(super) fn remove(
        service: &str,
        account: &str,
        require_tag: bool,
        eligible: impl FnOnce(&[u8]) -> Result<bool>,
    ) -> Result<()> {
        let keychain = (!require_tag)
            .then(SecKeychain::default)
            .transpose()
            .map_err(|_| unavailable())?;
        delete_verified(
            || exists(keychain.as_ref(), service, account),
            || {
                find_generic_password(
                    keychain.as_ref().map(std::slice::from_ref),
                    service,
                    account,
                )
                .map_err(|_| unavailable())
            },
            eligible,
            security_framework::os::macos::keychain_item::SecKeychainItem::delete,
        )
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use super::{Path, Result, unavailable};

    pub(super) fn read(_: Option<&Path>, _: &str, _: &str) -> Result<Option<Vec<u8>>> {
        Err(unavailable())
    }

    pub(super) fn contains(_: &str, _: &str) -> Result<bool> {
        Err(unavailable())
    }

    pub(super) fn without_interaction<T>(_: impl FnOnce() -> Result<T>) -> Result<T> {
        Err(unavailable())
    }

    pub(super) fn write_single(
        _: &str,
        _: &str,
        _: &[u8],
        _: bool,
        _: &dyn Fn() -> Result<()>,
        _: impl FnOnce(&[u8]) -> bool,
    ) -> Result<()> {
        Err(unavailable())
    }

    pub(super) fn add(_: &str, _: &str, _: &[u8], _: &dyn Fn() -> Result<()>) -> Result<()> {
        Err(unavailable())
    }

    pub(super) fn remove(
        _: &str,
        _: &str,
        _: bool,
        _: impl FnOnce(&[u8]) -> Result<bool>,
    ) -> Result<()> {
        Err(unavailable())
    }
}

#[cfg(test)]
#[path = "../tests/unit/recommend_keychain.rs"]
mod tests;
