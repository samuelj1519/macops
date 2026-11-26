use crate::{
    capability::rules::capability_tx::{Namespace, State},
    error::{Error, Result},
    platform::{fs::Directory, process},
    policy::{
        governance::DirectoryBinding,
        source_git::{ObjectId, Repository, TreeEntry, tree_entries},
    },
};
use std::{
    ffi::OsStr,
    fs,
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

pub struct Git {
    root: Directory,
    repo: PathBuf,
    home: PathBuf,
    temp: PathBuf,
    empty: PathBuf,
    hooks: PathBuf,
    directories: Vec<DirectoryBinding>,
    empty_witness: (Namespace, State),
    repo_witness: Option<(DirectoryBinding, Namespace, State)>,
    program: crate::domain::FileId,
    repository: Option<Repository>,
    storage_limit: u64,
}
fn invalid() -> Error {
    Error::attention(
        "unverified_git_source",
        "Git 来源准备未完成或对象与请求不符",
    )
}
impl Git {
    pub fn create(path: &Path) -> Result<Self> {
        crate::platform::fs::write_path(path)?;
        fs::create_dir(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        let root = Directory::open(path)?;
        let home = path.join("home");
        let temp = path.join("tmp");
        let hooks = path.join("hooks");
        for p in [&home, &temp, &hooks] {
            fs::create_dir(p)?;
            fs::set_permissions(p, fs::Permissions::from_mode(0o700))?;
        }
        let empty = path.join("empty-config");
        fs::write(&empty, b"")?;
        fs::set_permissions(&empty, fs::Permissions::from_mode(0o600))?;
        let directories = [&root.path, &home, &temp, &hooks]
            .into_iter()
            .map(|p| crate::capability::capability_store::binding(&Directory::open(p)?, p))
            .collect::<Result<Vec<_>>>()?;
        let empty_namespace = crate::capability::capability_store::namespace(&empty)?;
        let empty_state = crate::capability::capability_store::observe(&empty_namespace, 0)?.0;
        let program = crate::domain::FileId::from_file(&fs::File::open("/usr/bin/git")?)?;
        let mut git = Self {
            root,
            repo: path.join("objects.git"),
            home,
            temp,
            empty,
            hooks,
            directories,
            empty_witness: (empty_namespace, empty_state),
            repo_witness: None,
            program,
            repository: None,
            storage_limit: 256 * 1024 * 1024,
        };
        let args = vec![
            "init".into(),
            "--bare".into(),
            "--object-format=sha1".into(),
            git.repo.as_os_str().to_owned(),
        ];
        git.run(&args, 2 * 1024 * 1024)?;
        fs::set_permissions(&git.repo, fs::Permissions::from_mode(0o700))?;
        let repo = Directory::open(&git.repo)?;
        let config = crate::capability::capability_store::namespace(&git.repo.join("config"))?;
        let state = crate::capability::capability_store::observe(&config, 64 * 1024)?.0;
        git.repo_witness = Some((
            crate::capability::capability_store::binding(&repo, &git.repo)?,
            config,
            state,
        ));
        git.verify()?;
        Ok(git)
    }
    pub fn create_bound(path: &Path, repository: &Repository) -> Result<Self> {
        let mut git = Self::create(path)?;
        git.bind(repository)?;
        Ok(git)
    }
    pub fn require_repository(&self, repository: &Repository) -> Result<()> {
        self.verify()?;
        if self.repository.as_ref() != Some(repository) {
            return Err(invalid());
        }
        Ok(())
    }
    fn verify(&self) -> Result<()> {
        self.root.verify()?;
        for binding in &self.directories {
            let root = crate::capability::capability_store::open(&Namespace {
                ancestor: binding.clone(),
                remaining: vec![".probe".into()],
            })?;
            let meta = root.file().metadata()?;
            if meta.uid() != nix::unistd::geteuid().as_raw() || meta.mode() & 0o7777 != 0o700 {
                return Err(Error::attention(
                    "source_private_directory",
                    format!(
                        "Git 私有目录权限或所有者改变：{} uid={} mode={:o}",
                        root.path.display(),
                        meta.uid(),
                        meta.mode() & 0o7777
                    ),
                ));
            }
        }
        crate::capability::capability_store::cas(&self.empty_witness.0, &self.empty_witness.1)?;
        if let Some((binding, namespace, state)) = &self.repo_witness {
            crate::capability::capability_store::open(&Namespace {
                ancestor: binding.clone(),
                remaining: vec![".probe".into()],
            })?;
            crate::capability::capability_store::cas(namespace, state)?;
        }
        let meta = fs::symlink_metadata("/usr/bin/git")?;
        if !meta.is_file()
            || meta.uid() != 0
            || meta.mode() & 0o022 != 0
            || crate::domain::FileId::from_file(&fs::File::open("/usr/bin/git")?)? != self.program
        {
            return Err(Error::attention(
                "source_git_binary_changed",
                "受信 Git 程序身份或权限改变",
            ));
        }
        Ok(())
    }
    pub fn set_storage_budget(&mut self, remaining: u64) -> Result<()> {
        self.storage_limit = remaining.min(256 * 1024 * 1024);
        self.storage_usage().map(|_| ())
    }
    pub fn storage_usage(&self) -> Result<u64> {
        fn walk(
            root: &Directory,
            bytes: &mut u64,
            entries: &mut usize,
            limit: u64,
            depth: usize,
        ) -> Result<()> {
            if depth > 32 {
                return Err(invalid());
            }
            root.verify()?;
            for item in fs::read_dir(&root.path)? {
                let name = item?.file_name();
                *entries += 1;
                if *entries > 50000 {
                    return Err(invalid());
                }
                let metadata = match fs::symlink_metadata(root.path.join(&name)) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                if metadata.uid() != nix::unistd::geteuid().as_raw() {
                    return Err(Error::attention(
                        "source_storage_owner",
                        format!(
                            "Git 私有准备目录成员所有者未知：{}",
                            root.path.join(&name).display()
                        ),
                    ));
                }
                if metadata.is_dir() {
                    match root.child(&name) {
                        Ok(child) => walk(&child, bytes, entries, limit, depth + 1)?,
                        Err(error) if error.code == "path_missing" => (),
                        Err(error) => return Err(error),
                    }
                } else if metadata.is_file() || metadata.file_type().is_symlink() {
                    *bytes = bytes.checked_add(metadata.len()).ok_or_else(invalid)?;
                    if *bytes > limit {
                        return Err(Error::attention(
                            "source_storage_budget",
                            "Git 私有准备空间超过本次磁盘预算",
                        ));
                    }
                } else {
                    return Err(Error::attention(
                        "source_storage_type",
                        format!(
                            "Git 私有准备目录出现非普通成员：{}",
                            root.path.join(&name).display()
                        ),
                    ));
                }
            }
            root.verify()
        }
        let mut bytes = 0;
        let mut entries = 0;
        walk(&self.root, &mut bytes, &mut entries, self.storage_limit, 0)?;
        Ok(bytes)
    }
    fn command(&self, args: &[std::ffi::OsString]) -> Result<Command> {
        self.verify()?;
        let mut command = Command::new("/usr/bin/git");
        command
            .env_clear()
            .env("HOME", &self.home)
            .env("TMPDIR", &self.temp)
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &self.empty)
            .env("GIT_CONFIG_SYSTEM", &self.empty)
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_TEMPLATE_DIR", &self.hooks)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ALLOW_PROTOCOL", "https")
            .env("GIT_CONFIG_COUNT", "0")
            .env("LC_ALL", "C")
            .current_dir(&self.root.path)
            .stdin(Stdio::null())
            .process_group(0);
        command.args([
            OsStr::new("-c"),
            OsStr::new("core.fsmonitor=false"),
            OsStr::new("-c"),
            OsStr::new("protocol.allow=never"),
            OsStr::new("-c"),
            OsStr::new("protocol.https.allow=always"),
            OsStr::new("-c"),
            OsStr::new("http.followRedirects=false"),
        ]);
        command
            .arg("-c")
            .arg(format!("core.hooksPath={}", self.hooks.display()));
        command
            .arg("-c")
            .arg(format!("core.attributesFile={}", self.empty.display()));
        command.args(args);
        Ok(command)
    }
    fn run(&self, args: &[std::ffi::OsString], limit: u64) -> Result<Vec<u8>> {
        let output = process::capture_bytes_guarded(
            &mut self.command(args)?,
            Duration::from_secs(60),
            limit,
            || self.storage_usage().map(|_| ()),
        )?;
        self.verify()?;
        if output.code != Some(0) || output.uncertain {
            return Err(Error::attention(
                "unverified_git_source",
                format!(
                    "Git 来源查询失败：exit={:?}, uncertain={}, diagnostic={}",
                    output.code,
                    output.uncertain,
                    String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(1024)])
                ),
            ));
        }
        Ok(output.stdout)
    }
    fn object_command(&self, args: &[&str], limit: u64) -> Result<Vec<u8>> {
        let mut full = vec![
            std::ffi::OsString::from("--git-dir"),
            self.repo.as_os_str().to_owned(),
        ];
        full.extend(args.iter().map(std::ffi::OsString::from));
        self.run(&full, limit)
    }

    pub fn mirror_listing(&self, tree: &ObjectId) -> Result<Vec<u8>> {
        self.object_command(&["ls-tree", "-rlz", tree.as_str()], 16 * 1024 * 1024)
    }

    pub fn mirror_blobs(&self, objects: &[&ObjectId], limit: u64) -> Result<Vec<u8>> {
        use std::ffi::OsString;
        use std::io::{Seek as _, Write as _};
        let mut input = tempfile::tempfile_in(&self.temp)?;
        for object in objects {
            writeln!(input, "{}", object.as_str())?;
        }
        input.rewind()?;
        let args = [
            OsString::from("--git-dir"),
            self.repo.as_os_str().to_owned(),
            "cat-file".into(),
            "--batch".into(),
        ];
        let mut command = self.command(&args)?;
        command.stdin(Stdio::from(input));
        let output = crate::platform::process::capture_bytes_guarded(
            &mut command,
            Duration::from_secs(60),
            limit,
            || self.storage_usage().map(|_| ()),
        )?;
        self.verify()?;
        if output.code != Some(0) || output.uncertain {
            return Err(invalid());
        }
        Ok(output.stdout)
    }
    fn bind(&mut self, repository: &Repository) -> Result<()> {
        self.verify()?;
        if let Some(before) = &self.repository {
            return if before == repository {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        let bytes = format!(
            "[core]\n\trepositoryformatversion = 1\n\tfilemode = true\n\tbare = true\n[remote \"source\"]\n\turl = {}\n\tpromisor = true\n\tpartialclonefilter = blob:none\n",
            repository.url()
        );
        let (_, namespace, before) = self.repo_witness.as_ref().ok_or_else(invalid)?;
        let candidate =
            crate::capability::capability_store::stage(namespace, Some(bytes.as_bytes()), 0o600)?;
        crate::capability::capability_store::publish(namespace, before, &candidate)?;
        self.repo_witness.as_mut().ok_or_else(invalid)?.2 = candidate.after;
        self.repository = Some(repository.clone());
        self.verify()
    }
    pub fn resolve(
        &self,
        repository: &Repository,
        reference: &crate::policy::source_git::Reference,
    ) -> Result<ObjectId> {
        use crate::policy::source_git::{Reference, advertised_commit};
        if self
            .repository
            .as_ref()
            .is_some_and(|bound| bound != repository)
        {
            return Err(invalid());
        }
        if let Reference::Commit(id) = reference {
            return Ok(id.clone());
        }
        let mut arguments = vec![
            "ls-remote".to_owned(),
            "--symref".into(),
            "--exit-code".into(),
            "--".into(),
            repository.url(),
        ];
        arguments.extend(reference.patterns());
        let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        advertised_commit(reference, &self.object_command(&arguments, 64 * 1024)?)
    }
    pub fn fetch_commit(&mut self, repository: &Repository, commit: &ObjectId) -> Result<ObjectId> {
        self.bind(repository)?;
        self.object_command(
            &[
                "fetch",
                "--depth=1",
                "--no-tags",
                "--no-recurse-submodules",
                "--",
                "source",
                commit.as_str(),
            ],
            2 * 1024 * 1024,
        )?;
        self.fetched_tree(commit)
    }

    pub fn fetch_full_mirror(
        &self,
        repository: &Repository,
        commit: &ObjectId,
        branch: &str,
    ) -> Result<(ObjectId, &Path)> {
        self.fetch_full_mirror_using(repository, commit, branch, |git, argv| {
            git.object_command(argv, 2 * 1024 * 1024)
        })
    }

    fn fetch_full_mirror_using(
        &self,
        repository: &Repository,
        commit: &ObjectId,
        branch: &str,
        fetch: fn(&Self, &[&str]) -> Result<Vec<u8>>,
    ) -> Result<(ObjectId, &Path)> {
        use crate::policy::source_git::Reference;
        if self.repository.is_some()
            || !matches!(Reference::parse(Some(branch))?, Reference::Named(_))
            || branch.starts_with("refs/")
        {
            return Err(invalid());
        }
        fetch(
            self,
            &[
                "fetch",
                "--depth=1",
                "--no-tags",
                "--no-recurse-submodules",
                "--",
                &repository.url(),
                commit.as_str(),
            ],
        )?;
        self.pin_fetched_mirror(commit, branch)
    }

    fn pin_fetched_mirror(&self, commit: &ObjectId, branch: &str) -> Result<(ObjectId, &Path)> {
        let tree = self.fetched_tree(commit)?;
        let target = format!("refs/heads/{branch}");
        self.object_command(&["update-ref", &target, commit.as_str()], 1024)?;
        self.object_command(&["symbolic-ref", "HEAD", &target], 1024)?;
        Ok((tree, &self.repo))
    }

    fn fetched_tree(&self, commit: &ObjectId) -> Result<ObjectId> {
        let actual =
            self.object_command(&["rev-parse", "--verify", "FETCH_HEAD^{commit}"], 1024)?;
        let value = std::str::from_utf8(&actual)
            .map_err(|_| invalid())?
            .trim_end_matches('\n');
        let resolved = ObjectId::parse(value)?;
        if &resolved != commit {
            return Err(invalid());
        }
        let kind = self.object_command(&["cat-file", "-t", commit.as_str()], 1024)?;
        if kind != b"commit\n" {
            return Err(invalid());
        }
        let tree = self.object_command(&["rev-parse", "--verify", "FETCH_HEAD^{tree}"], 1024)?;
        ObjectId::parse(
            std::str::from_utf8(&tree)
                .map_err(|_| invalid())?
                .trim_end_matches('\n'),
        )
    }
    pub fn tree(&self, object: &ObjectId) -> Result<Vec<TreeEntry>> {
        if self.object_command(&["cat-file", "-t", object.as_str()], 1024)? != b"tree\n" {
            return Err(invalid());
        }
        tree_entries(&self.object_command(&["ls-tree", "-z", object.as_str()], 16 * 1024 * 1024)?)
    }
    pub fn blob(&self, object: &ObjectId) -> Result<Vec<u8>> {
        if self.object_command(&["cat-file", "-t", object.as_str()], 1024)? != b"blob\n" {
            return Err(invalid());
        }
        let size = self.object_command(&["cat-file", "-s", object.as_str()], 1024)?;
        let text = std::str::from_utf8(&size)
            .map_err(|_| invalid())?
            .trim_end_matches('\n');
        let length = text.parse::<u64>().map_err(|_| invalid())?;
        if length > 16 * 1024 * 1024 {
            return Err(invalid());
        }
        let bytes =
            self.object_command(&["cat-file", "blob", object.as_str()], 16 * 1024 * 1024)?;
        if bytes.len() as u64 != length {
            return Err(invalid());
        }
        Ok(bytes)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/source_git.rs"]
mod tests;
