use crate::{
    context::Context,
    domain::{Action, Observation, Package, Plan, Provider, fingerprint},
    error::{Error, Result},
    index_store::{BOOK_FILE, Book, Namespace, Registry},
    operations, paths,
    store::{Phase, Store},
};
use rustix::fs::{AtFlags, Mode, OFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs::File, io::Read, os::unix::fs::MetadataExt};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    schema_version: u32,
    files: BTreeMap<String, String>,
}
pub use crate::domain::Candidate;
fn fs_error(e: rustix::io::Errno) -> Error {
    std::io::Error::from_raw_os_error(e.raw_os_error()).into()
}
fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn candidate(root: &File, name: &str) -> Result<Candidate> {
    const LIMIT: u64 = 16 * 1024 * 1024;
    let file = File::from(
        rustix::fs::openat(
            root,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(fs_error)?,
    );
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(Error::new("cache_object_not_regular", name));
    }
    let mut bytes = vec![];
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(Error::new("cache_object_not_regular", name));
    }
    Ok(Candidate {
        name: name.into(),
        device: metadata.dev(),
        inode: metadata.ino(),
        sha256: format!("{:x}", Sha256::digest(bytes)),
    })
}

fn snapshot(namespace: &Namespace, book: &Book) -> Result<(Ownership, Vec<Candidate>)> {
    let mut owned = Ownership {
        schema_version: 1,
        files: BTreeMap::new(),
    };
    let mut candidates = vec![];
    for hash in &book.owned {
        let name = format!("{hash}.json");
        let item = match candidate(&namespace.indexes, &name) {
            Ok(v) => v,
            Err(e) if e.os_code == Some(rustix::io::Errno::NOENT.raw_os_error()) => {
                continue;
            }
            Err(e) => return Err(e),
        };
        crate::policy::cache::require_digest(hash, &item.sha256)?;
        owned.files.insert(name, hash.clone());
        candidates.push(item);
    }
    Ok((owned, candidates))
}

pub fn check_owned(ctx: &Context) -> Result<()> {
    crate::updates::read(ctx)?;
    Ok(())
}

pub fn cleanup(ctx: &Context, package: &Package, dry_run: bool, yes: bool) -> Result<Value> {
    if !matches!(package.provider, Provider::Macops) {
        return Err(Error::attention(
            "native_cache_owner",
            cache_advice(&package.provider),
        ));
    }
    let registry = Registry::load(ctx)?;
    let Some(namespace) = Namespace::open(ctx, false, &registry)? else {
        return Ok(
            json!({"mode":if dry_run {"dry-run"}else{"unchanged"},"candidates":[],"review":review_root(ctx)?}),
        );
    };
    namespace.assert_current(ctx, &registry)?;
    crate::policy::cache::registered_scope(&registry, &namespace.key, &ctx.cache)?;
    let book = registry.book(&namespace.path, &namespace.identity, &namespace.key);
    let (_, candidates) = snapshot(&namespace, &book)?;
    let review = review(&namespace, &book)?;
    if dry_run {
        return Ok(
            json!({"mode":"dry-run","candidates":candidates,"review":review,"persistentWrites":false}),
        );
    }
    if candidates.is_empty() && book.current.is_none() {
        return Ok(json!({"mode":"unchanged","candidates":[],"review":review}));
    }
    operations::confirm(
        &format!(
            "忘记本 scope 的版本缓存引用，清理 {} 个已验证 owned 索引；borrowed/unknown 保留",
            candidates.len()
        ),
        yes,
    )?;
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    let mut registry = Registry::load(ctx)?;
    namespace.assert_current(ctx, &registry)?;
    let mut book = registry.book(&namespace.path, &namespace.identity, &namespace.key);
    let (mut owned, current) = snapshot(&namespace, &book)?;
    crate::policy::cache::fresh_cleanup(&candidates, &current)?;
    let mut journal = cleanup_journal(ctx, package, &candidates)?;
    let quarantine = format!(".cleanup-{}", journal.id);
    journal.message = format!("quarantine={}/indexes/{}", ctx.cache.display(), quarantine);
    store.save(&journal)?;
    journal.phase = Phase::Running;
    store.save(&journal)?;
    book.current = None;
    registry.scopes.insert(namespace.key.clone(), book.clone());
    Store::atomic(&store.root, BOOK_FILE, &registry)?;
    let result = if candidates.is_empty() {
        Ok(())
    } else {
        purge(&namespace.indexes, &quarantine, &candidates, &mut owned)
    };
    crate::policy::cache::retain_remaining(&mut book, &owned.files);
    registry.scopes.insert(namespace.key.clone(), book);
    Store::atomic(&store.root, BOOK_FILE, &registry)?;
    match result {
        Ok(()) => {
            journal.phase = Phase::Succeeded;
            journal.message = format!(
                "已清理 {} 个owned索引，borrowed和unknown均保留",
                candidates.len()
            );
        }
        Err(e) => {
            journal.phase = Phase::Unknown;
            journal.message = format!(
                "{e}；保留 {}/indexes/{} 供审查",
                ctx.cache.display(),
                quarantine
            );
        }
    }
    store.save(&journal)?;
    Ok(json!({"journal":journal,"review":review}))
}

fn review(namespace: &Namespace, book: &Book) -> Result<Vec<String>> {
    Ok(crate::policy::cache::review_names(
        entries(&namespace.root)?,
        entries(&namespace.indexes)?,
        &book.owned,
    ))
}

fn review_root(ctx: &Context) -> Result<Vec<String>> {
    let path = paths::write_path(&ctx.cache)?;
    if !path.exists() {
        return Ok(vec![]);
    }
    let root =
        File::from(rustix::fs::open(&path, directory_flags(), Mode::empty()).map_err(fs_error)?);
    entries(&root)
}

fn entries(directory: &File) -> Result<Vec<String>> {
    let mut names = vec![];
    for entry in rustix::fs::Dir::read_from(directory).map_err(fs_error)? {
        let entry = entry.map_err(fs_error)?;
        let name = entry.file_name().to_string_lossy();
        if !matches!(name.as_ref(), "." | "..") {
            names.push(name.into_owned());
        }
        if names.len() >= 1000 {
            names.push("其余对象未扫描并保留".into());
            break;
        }
    }
    Ok(names)
}

fn purge(
    root: &File,
    quarantine: &str,
    candidates: &[Candidate],
    ownership: &mut Ownership,
) -> Result<()> {
    rustix::fs::mkdirat(root, quarantine, Mode::RWXU).map_err(fs_error)?;
    let isolated = File::from(
        rustix::fs::openat(root, quarantine, directory_flags(), Mode::empty()).map_err(fs_error)?,
    );
    for expected in candidates {
        rustix::fs::renameat_with(
            root,
            expected.name.as_str(),
            &isolated,
            expected.name.as_str(),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(fs_error)?;
        if !crate::policy::cache::candidate_matches(
            expected,
            &candidate(&isolated, &expected.name)?,
        ) {
            return Err(Error::attention(
                "quarantine_mismatch",
                "对象已被替换；保留隔离物，不覆盖新文件",
            ));
        }
        rustix::fs::unlinkat(&isolated, expected.name.as_str(), AtFlags::empty())
            .map_err(fs_error)?;
        ownership.files.remove(&expected.name);
    }
    rustix::fs::unlinkat(root, quarantine, AtFlags::REMOVEDIR).map_err(fs_error)?;
    root.sync_all()?;
    Ok::<_, Error>(())
}

fn cleanup_journal(
    ctx: &Context,
    package: &Package,
    candidates: &[Candidate],
) -> Result<crate::store::Journal> {
    operations::new_journal(
        ctx,
        Plan {
            brew: None,
            npm_binding: None,
            uv_owner: None,
            timeout_secs: ctx.timeout_secs,
            package: package.clone(),
            action: Action::Cleanup,
            before: Observation::Present {
                evidence: None,
                version: fingerprint(candidates)?,
            },
            target: None,
            command: None,
            query: None,
            recipe_sha256: fingerprint(package)?,
            warnings: vec!["中断后保留引用账本和quarantine，不重跑删除".into()],
            synchronous: true,
            delegated: false,
        },
    )
}

#[cfg(test)]
#[path = "../tests/unit/cache.rs"]
mod tests;

use crate::policy::cache::cache_advice;
