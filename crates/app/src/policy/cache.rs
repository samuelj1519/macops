use crate::domain::Provider;
pub fn cache_advice(provider: &Provider) -> String {
    let hint = match provider {
        Provider::Brew { .. } => "brew cleanup --dry-run；注意它可能覆盖共享下载和旧版本",
        Provider::Npm { .. } => {
            "npm cache verify 是校验维护；cache clean 没有可靠预演且需要 --force，不由本命令执行"
        }
        Provider::Bun { .. } => "先核对本机 bun pm cache --help；原生共享缓存可能被多个项目引用",
        Provider::Uv { .. } => "先查看 uv cache --help，再单独审查清理范围",
        Provider::Mise { .. } => "先查看 mise cache --help，再单独审查清理范围",
        _ => "没有已验证的单对象清理接口；保持原 owner，不直接删除目录",
    };
    format!("本命令只清理 macops 自有缓存。原生 owner 建议：{hint}")
}

use crate::{
    domain::{Book, Identity, Registry, fingerprint},
    error::{Error, Result},
};

pub fn require_identity(expected: &Identity, observed: &Identity) -> Result<()> {
    if expected != observed {
        return Err(Error::attention(
            "cache_scope_changed",
            "缓存目录实例或标记已改变",
        ));
    }
    Ok(())
}
pub fn validate_marker(marker: &crate::domain::Marker) -> Result<()> {
    if marker.format != "macops-index-scope-v1" || !valid_hash(&marker.scope_id) {
        return Err(Error::new("cache_scope_invalid", "缓存命名空间标记损坏"));
    }
    Ok(())
}
pub fn unchanged_marker(old: Option<&Book>, marker: Option<&crate::domain::Marker>) -> Result<()> {
    if old.is_some_and(|b| marker.is_none_or(|m| m.scope_id != b.identity.scope_id)) {
        return Err(Error::attention(
            "cache_scope_changed",
            "已登记目录的命名空间标记丢失或改变",
        ));
    }
    Ok(())
}
pub fn candidate_matches(
    expected: &crate::domain::Candidate,
    observed: &crate::domain::Candidate,
) -> bool {
    expected == observed
}
pub fn indexed_version(
    index: &crate::domain::Index,
    package: &crate::domain::Package,
    scope: &crate::domain::Scope,
) -> Result<Option<String>> {
    crate::policy::state::cache_version(package, scope, index.entries.get(&package.id))
}

pub fn register_current(book: &mut Book, hash: &str, created: bool) -> bool {
    book.current = Some(hash.to_owned());
    if created {
        book.owned.insert(hash.to_owned());
    }
    book.owned.contains(hash)
}

pub fn review_names(
    root_names: Vec<String>,
    index_names: Vec<String>,
    owned: &BTreeSet<String>,
) -> Vec<String> {
    let mut names = root_names
        .into_iter()
        .filter(|n| n != "indexes")
        .collect::<Vec<_>>();
    for name in index_names {
        if name != ".macops-scope.json" && !owned.iter().any(|h| name == format!("{h}.json")) {
            names.push(format!("indexes/{name}"));
        }
    }
    names.sort();
    names
}

pub fn apply_refresh(
    index: &mut crate::domain::Index,
    book: &mut Book,
    package: &crate::domain::Package,
    scope: &crate::domain::Scope,
    version: String,
    queried_at: u64,
) -> Result<()> {
    index.entries.insert(
        package.id.clone(),
        crate::domain::Available {
            version,
            source_sha256: fingerprint(package)?,
            scope_sha256: fingerprint(scope)?,
        },
    );
    book.queried_at.insert(package.id.clone(), queried_at);
    Ok(())
}

pub fn outdated_row(
    package: &crate::domain::Package,
    scope: &crate::domain::Scope,
    installed: &crate::domain::Observation,
    entry: Option<&crate::domain::Available>,
    queried_at: Option<u64>,
) -> Result<serde_json::Value> {
    let version = crate::policy::state::cache_version(package, scope, entry)?;
    Ok(if let Some(version) = version {
        serde_json::json!({"id":package.id,"installed":installed,"available":version,"queried_at":queried_at,"status":if installed.version()==Some(version.as_str()) {"current"}else{"different"}})
    } else {
        serde_json::json!({"id":package.id,"installed":installed,"available":null,"status":"unknown","hint":"先运行 mo app refresh；索引缺失或来源/作用域已改变"})
    })
}

pub fn registered_scope(registry: &Registry, key: &str, path: &std::path::Path) -> Result<()> {
    if !registry.scopes.contains_key(key) && registry.scopes.values().any(|b| b.path == path) {
        return Err(Error::attention(
            "cache_scope_changed",
            "不能把旧目录账本用于新目录",
        ));
    }
    Ok(())
}
pub fn require_digest(expected: &str, observed: &str) -> Result<()> {
    if expected != observed {
        return Err(Error::attention(
            "cache_modified",
            "已登记索引被修改，保留供审查",
        ));
    }
    Ok(())
}
pub fn fresh_cleanup(
    expected: &[crate::domain::Candidate],
    observed: &[crate::domain::Candidate],
) -> Result<()> {
    if expected != observed {
        return Err(Error::attention("cleanup_plan_stale", "候选对象已改变"));
    }
    Ok(())
}
pub fn retain_remaining(book: &mut Book, remaining: &BTreeMap<String, String>) {
    book.owned
        .retain(|hash| remaining.contains_key(&format!("{hash}.json")));
}
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Registry {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.format != "macops-index-book-v1" {
            return Err(Error::new("cache_book_schema", "缓存账本格式不支持"));
        }
        for (key, book) in &self.scopes {
            if identity_key(&book.identity) != *key
                || !valid_hash(&book.identity.scope_id)
                || book.current.as_ref().is_some_and(|h| !valid_hash(h))
                || book.owned.iter().any(|h| !valid_hash(h))
            {
                return Err(Error::new("cache_book_invalid", "缓存账本的身份或摘要无效"));
            }
        }
        Ok(())
    }

    pub fn book(&self, path: &Path, identity: &Identity, key: &str) -> Book {
        self.scopes.get(key).cloned().unwrap_or_else(|| Book {
            path: path.to_path_buf(),
            identity: identity.clone(),
            current: None,
            owned: BTreeSet::new(),
            queried_at: BTreeMap::new(),
        })
    }
}

pub fn identity_key(identity: &Identity) -> String {
    let scope = serde_json::Value::String(identity.scope_id.clone()).to_string();
    digest(format!("{{\"root\":{{\"device\":{},\"inode\":{}}},\"indexes\":{{\"device\":{},\"inode\":{}}},\"scope_id\":{scope}}}",identity.root.device,identity.root.inode,identity.indexes.device,identity.indexes.inode).as_bytes())
}

#[cfg(test)]
#[path = "../../tests/unit/cache_policy.rs"]
mod tests;
