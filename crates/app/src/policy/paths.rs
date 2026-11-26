use crate::error::{Error, Result};
use std::path::{Component, Path, PathBuf};

fn excluded_user_directory(path: &Path) -> bool {
    let mut parts = path.components();
    matches!(parts.next(), Some(Component::RootDir))
        && matches!(parts.next(), Some(Component::Normal(name)) if name.eq_ignore_ascii_case("Users"))
        && matches!(parts.next(), Some(Component::Normal(_)))
        && matches!(parts.next(), Some(Component::Normal(name)) if name.eq_ignore_ascii_case("job"))
}

pub fn lexical(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(Error::new(
            "absolute_path_required",
            path.display().to_string(),
        ));
    }
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                return Err(Error::new(
                    "parent_path_forbidden",
                    path.display().to_string(),
                ));
            }
            _ => out.push(part.as_os_str()),
        }
    }
    if excluded_user_directory(&out) {
        return Err(Error::new(
            "excluded_path",
            format!("mo 无法访问受保护的路径 {}", out.display()),
        ));
    }
    Ok(out)
}

pub fn normalize_link(path: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => (),
            _ => out.push(c.as_os_str()),
        }
        if excluded_user_directory(&out) {
            return Err(Error::new(
                "excluded_path",
                format!("链接指向受保护的目录 {}", out.display()),
            ));
        }
    }
    lexical(&out)
}

pub fn separate_roots(state: &Path, cache: &Path) -> Result<()> {
    if state == cache || state.starts_with(cache) || cache.starts_with(state) {
        return Err(Error::new(
            "overlapping_roots",
            "state 与 cache 必须是互不包含的独立目录",
        ));
    }
    Ok(())
}

pub fn empty_bun_root(
    scope: &crate::domain::Scope,
    output: &crate::domain::ProcessOutput,
) -> Option<PathBuf> {
    if output.uncertain || output.code != Some(1) {
        return None;
    }
    let root = scope
        .paths
        .get("BUN_INSTALL_GLOBAL_DIR")
        .and_then(Option::as_ref)
        .cloned()
        .unwrap_or_else(|| {
            scope
                .paths
                .get("BUN_INSTALL")
                .and_then(Option::as_ref)
                .cloned()
                .unwrap_or_else(|| scope.home.join(".bun"))
                .join("install/global")
        });
    output
        .stderr
        .contains(&format!(
            "No package.json was found for directory \"{}\"",
            root.display()
        ))
        .then_some(root)
}

pub fn path_candidates(name: &str, search: &std::ffi::OsStr) -> Vec<PathBuf> {
    if name.contains('/') {
        vec![PathBuf::from(name)]
    } else {
        std::env::split_paths(search)
            .filter(|p| p.is_absolute())
            .map(|p| p.join(name))
            .collect()
    }
}

pub fn legacy_app_path(raw: &str, scope: Option<&str>, home: &Path) -> Result<Option<PathBuf>> {
    if scope == Some("cask_non_app_artifact") && raw == "not_applicable" {
        return Ok(None);
    }
    legacy_path(raw, home).map(Some)
}

fn legacy_path(raw: &str, home: &Path) -> Result<PathBuf> {
    if let Some(relative) = raw.strip_prefix("~/") {
        lexical(&lexical(home)?.join(relative.trim_start_matches('/')))
    } else {
        lexical(Path::new(raw))
    }
}

#[cfg(test)]
#[path = "../../tests/unit/paths.rs"]
mod tests;
