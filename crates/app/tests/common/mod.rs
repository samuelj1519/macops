use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

pub type TestResult = Result<(), Box<dyn Error>>;

pub fn empty_environment_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_macops"));
    command.env_clear();
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    command
}

#[derive(Debug)]
pub struct Fixture {
    pub root: tempfile::TempDir,
    pub home: PathBuf,
    pub catalog: PathBuf,
}

impl Fixture {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let root = tempfile::Builder::new()
            .prefix("macops-test-")
            .tempdir_in("/private/tmp")?;
        let home = root.path().join("home");
        fs::create_dir_all(home.join("bin"))?;
        fs::create_dir_all(root.path().join("settings/manifests"))?;
        let catalog = root.path().join("catalog.json");
        let fixture = Self {
            root,
            home,
            catalog,
        };
        fixture.write_catalog()?;
        Ok(fixture)
    }

    pub fn script(&self, name: &str, body: &str) -> Result<PathBuf, Box<dyn Error>> {
        let p = self.home.join("bin").join(name);
        let fixed_home = self.home.to_string_lossy().replace('\'', "'\\''");
        fs::write(
            &p,
            format!("#!/bin/sh\nset -eu\nMACOPS_TEST_HOME='{fixed_home}'\n{body}\n"),
        )?;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755))?;
        Ok(p)
    }

    pub fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_macops"));
        c.args(["--json", "--home"])
            .arg(&self.home)
            .arg("--settings")
            .arg(self.root.path().join("settings"))
            .arg("--state-dir")
            .arg(self.root.path().join("state"))
            .arg("--cache-dir")
            .arg(self.root.path().join("cache"))
            .arg("--catalog")
            .arg(&self.catalog)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.home.join("bin").display()),
            );
        c
    }

    pub fn run(&self, args: &[&str]) -> Result<(Output, Value), Box<dyn Error>> {
        let out = self.command().args(args).output()?;
        let value = serde_json::from_slice(&out.stdout).map_err(|e| {
            format!(
                "{e}: stdout={} stderr={}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })?;
        Ok((out, value))
    }

    pub fn write_catalog(&self) -> Result<(), Box<dyn Error>> {
        let program=self.script("demo",r#"case "$1" in
query) if [ -f "$2" ]; then printf '{"status":"present","version":"%s"}\n' "$(/bin/cat "$2")"; else printf '{"status":"missing"}\n'; fi ;;
available) printf '{"version":"2.0.0"}\n' ;;
install|upgrade) printf '%s' "$4" > "$2"; printf '%s\n' "$1" >> "$3" ;;
uninstall) /bin/rm "$2"; printf 'uninstall\n' >> "$3" ;;
*) exit 8 ;;
esac"#)?;
        let version = self.root.path().join("version");
        let log = self.root.path().join("calls");
        let recipe = json!({"schema_version":1,"packages":[{"id":"fixture:demo","name":"demo","kind":"cli","official_url":"https://example.org/demo","provider":{"type":"command","query":{"program":program,"args":["query",version]},"available":{"program":program,"args":["available"]},"install":{"program":program,"args":["install",version,log,"{version}"]},"upgrade":{"program":program,"args":["upgrade",version,log,"{version}"]},"uninstall":{"program":program,"args":["uninstall",version,log]},"synchronous":true}}]});
        fs::write(&self.catalog, serde_json::to_vec_pretty(&recipe)?)?;
        Ok(())
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
    pub fn read_json(&self, name: &str) -> Result<Value, Box<dyn Error>> {
        Ok(serde_json::from_slice(&fs::read(self.file(name))?)?)
    }

    pub fn current_index(&self) -> Result<PathBuf, Box<dyn Error>> {
        let book = self.read_json("state/version-indexes.json")?;
        let current = book["scopes"]
            .as_object()
            .and_then(|s| s.values().find_map(|v| v["current"].as_str()))
            .ok_or("no current index")?;
        Ok(self.file("cache/indexes").join(format!("{current}.json")))
    }
}

pub fn assert_code(out: &Output, code: i32) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
pub fn write_json(path: &Path, value: &Value) -> Result<(), Box<dyn Error>> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

impl Fixture {
    pub fn roots_command(&self) -> Command {
        let mut command = empty_environment_command();
        command
            .env("HOME", &self.home)
            .args(["--json", "--settings"])
            .arg(self.file("settings"));
        command
    }
}

pub fn uv_tool(root: &Path, bin: &Path, name: &str, version: &str) -> TestResult {
    let env = root.join(name);
    let dist = env.join(format!(
        "lib/python3.12/site-packages/{}-{version}.dist-info",
        name.replace('-', "_")
    ));
    fs::create_dir_all(&dist)?;
    fs::create_dir_all(env.join("bin"))?;
    fs::create_dir_all(bin)?;
    fs::write(
        dist.join("METADATA"),
        format!("Name: {name}\nVersion: {version}\n"),
    )?;
    fs::write(
        env.join("pyvenv.cfg"),
        "home = /synthetic-unexecuted-python\n",
    )?;
    fs::write(
        env.join("uv-receipt.toml"),
        format!(
            "[tool]\nrequirements=[{{name='{name}'}}]\nentrypoints=[{{name='{name}',install-path='{}'}}]\n",
            bin.join(name).display()
        ),
    )?;
    fs::write(
        env.join("bin").join(name),
        "#!/synthetic-unexecuted-python\nexit 99\n",
    )?;
    std::os::unix::fs::symlink(env.join("bin").join(name), bin.join(name))?;
    std::os::unix::fs::symlink("/synthetic-unexecuted-python", env.join("bin/python"))?;
    Ok(())
}
