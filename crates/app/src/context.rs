use crate::{
    cli::Cli,
    error::{Error, Result},
    paths,
};
use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub struct HarnessRoots {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
}
impl HarnessRoots {
    fn from_values(
        claude: Option<PathBuf>,
        codex: Option<PathBuf>,
        isolated: bool,
        warnings: &mut Vec<String>,
    ) -> Result<Self> {
        let mut parse = |key: &str, value: Option<PathBuf>| {
            let (value, warning) = crate::policy::layout::scope_value(
                key,
                if isolated {
                    None
                } else {
                    value.as_deref().filter(|p| !p.as_os_str().is_empty())
                },
            );
            warnings.extend(warning);
            if let Some(path) = &value {
                paths::read_path(path)?;
            }
            Ok::<_, Error>(value)
        };
        Ok(Self {
            claude: parse("CLAUDE_CONFIG_DIR", claude)?,
            codex: parse("CODEX_HOME", codex)?,
        })
    }
    fn capture(isolated: bool, warnings: &mut Vec<String>) -> Result<Self> {
        Self::from_values(
            std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
            std::env::var_os("CODEX_HOME").map(PathBuf::from),
            isolated,
            warnings,
        )
    }
}
fn logical_home(cli: &Cli) -> Result<(PathBuf, bool)> {
    let ambient = std::env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    if let Some(home) = &cli.home {
        return Ok((home.clone(), ambient.as_ref() != Some(home)));
    }
    ambient
        .map(|home| (home, false))
        .ok_or_else(|| Error::new("home_missing", "请使用 --home 指定绝对路径"))
}

#[derive(Debug)]
pub(crate) struct RecommendSetupContext {
    pub(crate) home: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) timeout_secs: u64,
    pub(crate) isolated: bool,
}

impl RecommendSetupContext {
    pub(crate) fn from_cli(cli: &Cli, warnings: &mut Vec<String>) -> Result<Self> {
        let (home, isolated) = logical_home(cli)?;
        paths::read_path(&home)?;
        let base = (!isolated)
            .then(|| std::env::var_os("XDG_CONFIG_HOME"))
            .flatten()
            .map(PathBuf::from);
        let (base, warning) =
            crate::policy::layout::scope_value("XDG_CONFIG_HOME", base.as_deref());
        warnings.extend(warning);
        if let Some(base) = &base {
            validate_scope_path("XDG_CONFIG_HOME", base)?;
        }
        let config = crate::policy::layout::app_root(
            cli.config_dir.as_deref(),
            base.as_deref(),
            &home,
            ".config/macops",
        )?;
        validate_app_root(&config)?;
        Ok(Self {
            home,
            config,
            timeout_secs: cli.timeout_secs,
            isolated,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Context {
    pub scope: crate::domain::Scope,
    pub harness: HarnessRoots,
    pub uv: crate::uv_local::DiscoveryInputs,
    pub timeout_secs: u64,
    pub home: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub settings: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
    pub catalog: PathBuf,
    pub catalog_explicit: bool,
    pub isolated: bool,
    pub chezmoi: crate::config_inventory::Inputs,
}

impl Context {
    pub fn from_cli(cli: &Cli, warnings: &mut Vec<String>) -> Result<Self> {
        let (home, isolated) = logical_home(cli)?;
        let scope = scope(&home, isolated, warnings)?;
        let root = |explicit: Option<&std::path::Path>, key: &str, fallback: &str| {
            crate::policy::layout::app_root(
                explicit,
                scope.paths.get(key).and_then(Option::as_deref),
                &home,
                fallback,
            )
        };
        let config = root(
            cli.config_dir.as_deref(),
            "XDG_CONFIG_HOME",
            ".config/macops",
        )?;
        let context = Self {
            config: config.clone(),
            harness: HarnessRoots::capture(isolated, warnings)?,
            uv: crate::uv_local::DiscoveryInputs::capture(isolated),
            data: root(
                cli.data_dir.as_deref(),
                "XDG_DATA_HOME",
                ".local/share/macops",
            )?,
            state: root(
                cli.state_dir.as_deref(),
                "XDG_STATE_HOME",
                ".local/state/macops",
            )?,
            cache: root(cli.cache_dir.as_deref(), "XDG_CACHE_HOME", ".cache/macops")?,
            catalog: cli
                .catalog
                .clone()
                .unwrap_or_else(|| config.join("catalog.json")),
            scope,
            timeout_secs: cli.timeout_secs,
            settings: cli
                .settings
                .clone()
                .unwrap_or_else(|| home.join(".local/settings")),
            catalog_explicit: cli.catalog.is_some(),
            isolated,
            chezmoi: crate::config_inventory::Inputs::capture(cli, isolated),
            home,
        };
        context.validate_roots(cli.settings.is_some())?;
        context.prevent_state_switch(cli.state_dir.is_some())?;
        Ok(context)
    }
    fn validate_roots(&self, explicit_settings: bool) -> Result<()> {
        for path in [
            &self.home,
            &self.config,
            &self.data,
            &self.settings,
            &self.state,
            &self.cache,
            &self.catalog,
        ] {
            paths::read_path(path)?;
        }
        for root in [&self.config, &self.data, &self.state, &self.cache] {
            validate_app_root(root)?;
        }
        crate::policy::paths::separate_roots(&self.state, &self.cache)?;
        if explicit_settings && !self.settings.is_dir() {
            return Err(Error::new(
                "settings_missing",
                self.settings.display().to_string(),
            ));
        }
        Ok(())
    }

    fn prevent_state_switch(&self, explicit_state: bool) -> Result<()> {
        let old = crate::policy::paths::lexical(&self.home.join(".local/state/macops"))?;
        if !explicit_state && old != self.state {
            paths::read_path(&old)?;
            let old_exists = match std::fs::symlink_metadata(&old) {
                Ok(metadata) if metadata.is_dir() => true,
                Ok(_) => {
                    return Err(Error::new(
                        "invalid_state_root",
                        "旧 state 必须是普通目录，不能绕过异常旧状态",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.into()),
            };
            if crate::policy::layout::state_migration_required(false, &old, &self.state, old_exists)
            {
                return Err(Error::attention(
                    "state_layout_migration_required",
                    format!(
                        "旧状态目录 {} 已存在；使用 --state-dir {} 查看历史，并通过数据转换切换目录",
                        old.display(),
                        old.display(),
                    ),
                ));
            }
        }
        Ok(())
    }
}

fn validate_app_root(root: &std::path::Path) -> Result<()> {
    match std::fs::metadata(paths::read_path(root)?) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(Error::new("root_not_directory", root.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_scope_path(key: &str, path: &std::path::Path) -> Result<()> {
    paths::read_path(path).map(|_| ()).map_err(|_| {
        Error::new(
            "invalid_scope_path",
            format!("作用域环境变量 {key} 必须是允许的绝对路径"),
        )
    })
}

fn scope(
    home: &std::path::Path,
    isolated: bool,
    warnings: &mut Vec<String>,
) -> Result<crate::domain::Scope> {
    let mut paths = std::collections::BTreeMap::new();
    for key in [
        "MISE_DATA_DIR",
        "MISE_CONFIG_DIR",
        "MISE_CONFIG_FILE",
        "MISE_GLOBAL_CONFIG_FILE",
        "UV_TOOL_DIR",
        "UV_TOOL_BIN_DIR",
        "UV_PYTHON_INSTALL_DIR",
        "UV_CACHE_DIR",
        "BUN_INSTALL",
        "BUN_INSTALL_GLOBAL_DIR",
        "NPM_CONFIG_PREFIX",
        "NPM_CONFIG_USERCONFIG",
        "npm_config_prefix",
        "npm_config_userconfig",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
    ] {
        let path = if isolated {
            None
        } else {
            std::env::var_os(key).map(PathBuf::from)
        };
        let (path, warning) = crate::policy::layout::scope_value(key, path.as_deref());
        warnings.extend(warning);
        if let Some(ref p) = path {
            validate_scope_path(key, p)?;
        }
        paths.insert(key.into(), path);
    }
    Ok(crate::domain::Scope {
        home: home.to_path_buf(),
        paths,
        service_identity: None,
    })
}

#[cfg(test)]
#[path = "../tests/unit/harness_context.rs"]
mod harness_tests;
