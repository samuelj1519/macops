use crate::{
    agent::native_plugins::{Instance, RECIPE_FILE, Recipe, Registry},
    cli::Cli,
    context::Context,
    error::Result,
    test_support::TestResult,
};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub struct Fixture {
    root: tempfile::TempDir,
    pub home: PathBuf,
    pub config: PathBuf,
    pub source: PathBuf,
    pub cwd: PathBuf,
    pub state: PathBuf,
    pub binary: PathBuf,
}

impl Fixture {
    pub fn new() -> Result<Self> {
        let root = tempfile::Builder::new()
            .prefix("macops-plugin-probe-")
            .tempdir_in("/private/tmp")?;
        let home = root.path().join("home");
        let config = home.join("codex-owned");
        let source = root.path().join("market");
        let cwd = root.path().join("untrusted-project-free-cwd");
        let state = root.path().join("state");
        let binary = root.path().join("unexecuted binary");
        for directory in [&home, &config, &source, &cwd] {
            fs::create_dir(directory)?;
        }
        fs::write(&binary, b"#!/bin/sh\nexit 91\n")?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o500))?;
        Ok(Self {
            root,
            home,
            config,
            source,
            cwd,
            state,
            binary,
        })
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn instance_json(&self) -> Value {
        json!({
            "home":self.home,
            "config_root":self.config,
            "cwd":self.cwd,
            "binary":self.binary,
            "binary_sha256":format!("{:x}", Sha256::digest(b"#!/bin/sh\nexit 91\n")),
            "version":"0.169.0"
        })
    }

    pub fn recipe_json(&self) -> Value {
        json!({
            "revision":"accepted-r1",
            "instances":["codex"],
            "marketplace":"market",
            "plugin":"example",
            "source":{"type":"local_directory","root":self.source,"member":"plugins/example"}
        })
    }

    pub fn book(&self) -> Value {
        json!({"schema_version":1,"instances":{"codex":self.instance_json()},"recipes":{"example":self.recipe_json()}})
    }

    pub fn binding(&self) -> Result<(Instance, Recipe)> {
        let mut registry = Registry::parse(&serde_json::to_vec(&self.book())?)?;
        Ok((
            registry
                .instances
                .remove("codex")
                .ok_or_else(|| crate::error::Error::new("test_fixture", "codex"))?,
            registry
                .recipes
                .remove("example")
                .ok_or_else(|| crate::error::Error::new("test_fixture", "recipe"))?,
        ))
    }

    pub fn declaration(&self) -> PathBuf {
        self.home.join(".config/macops").join(RECIPE_FILE)
    }

    pub fn write_book(&self, book: &Value) -> TestResult {
        write_json(&self.declaration(), book)
    }

    pub fn context(&self) -> Check<Context> {
        let cli = Cli::try_parse_from([
            "macops".as_ref(),
            "--home".as_ref(),
            self.home.as_os_str(),
            "--state-dir".as_ref(),
            self.state.as_os_str(),
            "skill".as_ref(),
            "plugins".as_ref(),
            "list".as_ref(),
        ])?;
        Ok(Context::from_cli(&cli, &mut vec![])?)
    }

    pub fn source_member(&self) -> PathBuf {
        self.source.join("plugins/example")
    }

    pub fn cache(&self) -> PathBuf {
        self.config.join("plugins/cache/market/example")
    }

    pub fn codex_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    pub fn market_entry(config_origin: &Path, plugin: &str) -> String {
        format!(
            "[marketplaces.market]\nsource_type='local'\nsource='{}'\n{plugin}",
            config_origin.display()
        )
    }

    pub fn installed(&self) -> TestResult {
        fs::write(
            self.codex_file(),
            Self::market_entry(&self.source, "[plugins.'example@market']\nenabled=true\n"),
        )?;
        let payload = json!({"name":"example","version":"1.2.3"});
        write_json(
            &self.source_member().join(".codex-plugin/plugin.json"),
            &payload,
        )?;
        write_json(
            &self.cache().join("1.2.3/.codex-plugin/plugin.json"),
            &payload,
        )?;
        write_json(
            &self.source.join(".agents/plugins/marketplace.json"),
            &json!({"name":"market","plugins":[{"name":"example","source":{"source":"local","path":"./plugins/example"}}]}),
        )
    }
}

pub type Check<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn write_json(path: &Path, value: &Value) -> TestResult {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec(value)?)?;
    Ok(())
}

pub fn error<T: std::fmt::Debug>(result: Result<T>, code: &str) {
    let actual = result.map_err(|failure| failure.code);
    assert!(matches!(&actual, Err(found) if found == code), "{actual:?}");
}
