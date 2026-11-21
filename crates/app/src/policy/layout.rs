use crate::error::Result;
use std::path::{Path, PathBuf};

pub fn scope_value(key: &str, value: Option<&Path>) -> (Option<PathBuf>, Option<String>) {
    if key.starts_with("XDG_") {
        match value {
            None => (None, None),
            Some(path) if path.as_os_str().is_empty() => (None, None),
            Some(path) if !path.is_absolute() => (
                None,
                Some(format!(
                    "忽略非绝对路径环境变量 {key}，使用逻辑 HOME 的默认目录"
                )),
            ),
            Some(path) => (Some(path.to_path_buf()), None),
        }
    } else {
        (value.map(Path::to_path_buf), None)
    }
}

pub fn app_root(
    explicit: Option<&Path>,
    base: Option<&Path>,
    home: &Path,
    fallback: &str,
) -> Result<PathBuf> {
    let path = explicit.map_or_else(
        || base.map_or_else(|| home.join(fallback), |base| base.join("macops")),
        Path::to_path_buf,
    );
    crate::policy::paths::lexical(&path)
}

pub fn state_migration_required(explicit: bool, old: &Path, new: &Path, old_exists: bool) -> bool {
    !explicit && old != new && old_exists
}

#[cfg(test)]
#[path = "../../tests/unit/layout.rs"]
mod tests;
