use crate::{
    context::Context,
    error::{Error, Result},
    operations::store::Store,
    platform::fs as paths,
};
use rustix::fs::{Mode, OFlags};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::PathBuf,
};

pub const BOOK_FILE: &str = "version-indexes.json";
const MARKER: &str = ".macops-scope.json";

pub use crate::domain::{Book, FileId, Identity, Marker, Registry};
impl FileId {
    pub fn from_file(file: &File) -> Result<Self> {
        let m = file.metadata()?;
        Ok(Self {
            device: m.dev(),
            inode: m.ino(),
        })
    }
}

#[derive(Debug)]
pub struct Namespace {
    pub root: File,
    pub indexes: File,
    pub identity: Identity,
    pub path: PathBuf,
    pub key: String,
}

fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
fn fs_error(e: rustix::io::Errno) -> Error {
    std::io::Error::from_raw_os_error(e.raw_os_error()).into()
}

impl Registry {
    pub fn load(ctx: &Context) -> Result<Self> {
        let path = ctx.state.join(BOOK_FILE);
        let value = if paths::read_path(&path)?.exists() {
            paths::regular_json(&path)?
        } else {
            Self {
                schema_version: 1,
                format: "macops-index-book-v1".into(),
                scopes: BTreeMap::new(),
            }
        };
        value.validate()?;
        Ok(value)
    }
}

impl Namespace {
    pub fn open(ctx: &Context, create: bool, registry: &Registry) -> Result<Option<Self>> {
        let path = paths::write_path(&ctx.cache)?;
        if !path.exists() {
            if !create {
                return Ok(None);
            }
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&path)?;
        }
        let root = File::from(rustix::fs::open(&path, flags(), Mode::empty()).map_err(fs_error)?);
        if create {
            match rustix::fs::mkdirat(&root, "indexes", Mode::RWXU) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => (),
                Err(e) => return Err(fs_error(e)),
            }
        }
        let indexes = match rustix::fs::openat(&root, "indexes", flags(), Mode::empty()) {
            Ok(fd) => File::from(fd),
            Err(rustix::io::Errno::NOENT) if !create => return Ok(None),
            Err(e) => return Err(fs_error(e)),
        };
        let root_id = FileId::from_file(&root)?;
        let index_id = FileId::from_file(&indexes)?;
        let mut marker = Store::read::<Marker>(&indexes, MARKER)?;
        let old = registry
            .scopes
            .values()
            .find(|b| b.identity.root == root_id && b.identity.indexes == index_id);
        crate::policy::catalog::cache::unchanged_marker(old, marker.as_ref())?;
        if marker.is_none() && create {
            let mut random = [0_u8; 32];
            getrandom::fill(&mut random)
                .map_err(|e| Error::new("entropy_unavailable", e.to_string()))?;
            let next = Marker {
                format: "macops-index-scope-v1".into(),
                scope_id: digest(&random),
            };
            match Store::atomic_new(&indexes, MARKER, &next) {
                Ok(()) => (),
                Err(e) if e.code == "already_exists" => (),
                Err(e) => return Err(e),
            }
            marker = Store::read(&indexes, MARKER)?;
        }
        let scope_id = if let Some(marker) = marker {
            crate::policy::catalog::cache::validate_marker(&marker)?;
            marker.scope_id
        } else {
            String::new()
        };
        let identity = Identity {
            root: root_id,
            indexes: index_id,
            scope_id,
        };
        if create {
            indexes.sync_all()?;
            root.sync_all()?;
            if let Some(parent) = path.parent() {
                let parent = paths::write_path(parent)?;
                File::from(rustix::fs::open(&parent, flags(), Mode::empty()).map_err(fs_error)?)
                    .sync_all()?;
            }
        }
        let key = crate::policy::catalog::cache::identity_key(&identity);
        Ok(Some(Self {
            root,
            indexes,
            identity,
            path,
            key,
        }))
    }

    pub fn assert_current(&self, ctx: &Context, registry: &Registry) -> Result<()> {
        let current = Self::open(ctx, false, registry)?
            .ok_or_else(|| Error::attention("cache_scope_changed", "缓存目录已消失"))?;
        crate::policy::catalog::cache::require_identity(&self.identity, &current.identity)
    }

    pub fn read_blob<T: DeserializeOwned>(&self, hash: &str) -> Result<T> {
        if !valid_hash(hash) {
            return Err(Error::new("invalid_cache_hash", hash));
        }
        let name = format!("{hash}.json");
        let bytes = read_bytes(&self.indexes, &name)?;
        if digest(&bytes) != hash {
            return Err(Error::attention(
                "cache_modified",
                "索引内容与登记摘要不符，保留供审查",
            ));
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn publish<T: Serialize>(&self, value: &T) -> Result<(String, bool)> {
        let mut bytes = serde_json::to_vec_pretty(value)?;
        bytes.push(b'\n');
        let hash = digest(&bytes);
        let name = format!("{hash}.json");
        match Store::atomic_bytes(&self.indexes, &name, &bytes, true) {
            Ok(()) => Ok((hash, true)),
            Err(e) if e.code == "already_exists" => {
                let existing = read_bytes(&self.indexes, &name)?;
                if existing != bytes {
                    return Err(Error::attention(
                        "cache_collision",
                        "同摘要名已有不同内容，未覆盖",
                    ));
                }
                let file = File::from(
                    rustix::fs::openat(
                        &self.indexes,
                        name.as_str(),
                        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .map_err(fs_error)?,
                );
                file.sync_all()?;
                self.indexes.sync_all()?;
                Ok((hash, false))
            }
            Err(e) => Err(e),
        }
    }
}

pub fn read_bytes(directory: &File, name: &str) -> Result<Vec<u8>> {
    let file = File::from(
        rustix::fs::openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(fs_error)?,
    );
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > 16 * 1024 * 1024 {
        return Err(Error::new("invalid_cache_file", name));
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(Error::new("invalid_cache_file", "缓存读取超过限制"));
    }
    Ok(bytes)
}

pub use crate::policy::catalog::cache::{digest, valid_hash};
