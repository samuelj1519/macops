use super::{
    native_skills_files as disk,
    rules::native_skills::{self as policy, Recipe, Runner, Source},
};
use crate::{
    context::Context,
    error::{Error, Result},
    policy::source_git::{ObjectId, Reference, Repository, TreeKind},
    source::source_git::Git,
};
use serde_json::Value;
use std::{collections::BTreeSet, path::Path};

pub(super) struct Request {
    pub repository: Repository,
    reference: Reference,
    tracking_ref: Option<String>,
}

impl Request {
    pub fn parse(input: &str) -> Result<Self> {
        let (source, reference) = input
            .rsplit_once('@')
            .map_or((input, None), |(source, reference)| {
                (source, Some(reference))
            });
        let repository = crate::policy::public_source::repository_subject(
            source.strip_prefix("github.com/").unwrap_or(source),
        )
        .and_then(|repo| Repository::parse(repo).ok())
        .ok_or_else(|| {
            Error::new(
                "skill_source_invalid",
                "请提供 GitHub 仓库，例如 owner/repo。",
            )
            .with_guidance(
                "无法识别技能来源。",
                "运行 mo skill install owner/repo --dry-run 查看技能。",
            )
        })?;
        let parsed = Reference::parse(reference)?;
        let tracking_ref = matches!(parsed, Reference::Head).then(|| "HEAD".to_owned());
        Ok(Self {
            repository,
            reference: parsed,
            tracking_ref,
        })
    }

    pub fn resolve(&self) -> Result<ObjectId> {
        with_git(&self.repository, |git| {
            git.resolve(&self.repository, &self.reference)
        })
    }

    pub(super) fn claude_offer_at(
        &self,
        commit: &ObjectId,
    ) -> Result<Option<crate::agent::claude_install_source::Offer>> {
        with_git(&self.repository, |git| {
            let root = git.fetch_commit(&self.repository, commit)?;
            self.claude_offer_from_tree(git, &root, commit)
        })
    }

    fn claude_offer_from_tree(
        &self,
        git: &Git,
        root: &ObjectId,
        commit: &ObjectId,
    ) -> Result<Option<crate::agent::claude_install_source::Offer>> {
        let Some(dir) = subtree(git, root, ".claude-plugin")? else {
            return Ok(None);
        };
        let files = git.tree(&dir)?;
        let blob = |name: &str| {
            files
                .iter()
                .find(|file| {
                    file.name.as_str() == name && matches!(file.kind, TreeKind::File { .. })
                })
                .map(|file| git.blob(&file.object))
                .transpose()
        };
        let (Some(marketplace), Some(plugin)) = (blob("marketplace.json")?, blob("plugin.json")?)
        else {
            return Ok(None);
        };
        crate::agent::claude_install_source::Offer::identify(
            self.repository.key(),
            commit,
            &marketplace,
            &plugin,
        )
        .map(Some)
    }

    pub fn discover(&self, ctx: &Context) -> Result<Vec<Recipe>> {
        let runner = runner(ctx)?;
        with_git(&self.repository, |git| {
            let commit = git.resolve(&self.repository, &self.reference)?;
            let root = git.fetch_commit(&self.repository, &commit)?;
            self.skills_from_tree(git, &root, &commit, &runner)
        })
    }

    fn skills_from_tree(
        &self,
        git: &Git,
        root: &ObjectId,
        commit: &ObjectId,
        runner: &Runner,
    ) -> Result<Vec<Recipe>> {
        let mut sources = vec![];
        read_member(git, root, ".", &mut sources)?;
        let mut inspected = 1;
        for folder in ["skills", ".agents/skills"] {
            let Some(directory) = subtree(git, root, folder)? else {
                continue;
            };
            let mut members = git.tree(&directory)?;
            members.retain(|entry| entry.kind == TreeKind::Directory);
            inspected += members.len();
            if inspected > 128 {
                return Err(Error::attention(
                    "public_skill_member_limit",
                    "仓库的技能目录超过核对上限。请运行 mo recommend 查看来源提供的安装方式。",
                ));
            }
            for entry in members {
                let path = format!("{folder}/{}", entry.name.as_str());
                read_member(git, &entry.object, &path, &mut sources)?;
            }
        }
        let mut names = BTreeSet::new();
        if sources.is_empty()
            || sources
                .iter()
                .any(|(_, name, _)| !names.insert(name.clone()))
        {
            return Err(Error::attention(
                "public_skill_members_unavailable",
                "在该版本的技能目录中找不到唯一可安装的技能。",
            )
            .with_guidance(
                "没有可选的技能。",
                format!(
                    "运行 mo recommend {} 查看来源提供的安装方式。",
                    self.repository.key()
                ),
            ));
        }
        sources
            .into_iter()
            .map(|(directory, name, manual)| {
                let source = Source {
                    repository: self.repository.key().into(),
                    commit: commit.as_str().into(),
                    skill_directory: directory,
                    member_name: name.clone(),
                    tracking_ref: self.tracking_ref.clone(),
                };
                recipe(name, source, manual, runner.clone())
            })
            .collect()
    }
}

fn with_git<T>(repository: &Repository, read: impl FnOnce(&mut Git) -> Result<T>) -> Result<T> {
    let parent = crate::platform::fs::read_path(&std::env::temp_dir())?;
    let scratch = tempfile::Builder::new()
        .prefix("macops-skill-source-")
        .tempdir_in(parent)?;
    let mut git = Git::create_bound(&scratch.path().join("git"), repository)?;
    git.set_storage_budget(256 * 1024 * 1024)?;
    read(&mut git)
}

fn subtree(git: &Git, root: &ObjectId, path: &str) -> Result<Option<ObjectId>> {
    let mut current = root.clone();
    for name in path.split('/') {
        let Some(entry) = git
            .tree(&current)?
            .into_iter()
            .find(|entry| entry.name.as_str() == name && entry.kind == TreeKind::Directory)
        else {
            return Ok(None);
        };
        current = entry.object;
    }
    Ok(Some(current))
}

fn read_member(
    git: &Git,
    tree: &ObjectId,
    path: &str,
    members: &mut Vec<(String, String, bool)>,
) -> Result<()> {
    if let Some(entry) = git.tree(tree)?.into_iter().find(|entry| {
        entry.name.as_str() == "SKILL.md" && matches!(entry.kind, TreeKind::File { .. })
    }) {
        let raw = git.blob(&entry.object)?;
        let name = crate::capability::rules::invocation::skill_name(&raw)?;
        let manual = crate::capability::rules::invocation::manual_frontmatter(&raw)?;
        members.push((path.into(), name, manual));
    }
    Ok(())
}

pub(super) fn source_of(row: &Value) -> Option<Repository> {
    (row["sourceType"] == "github")
        .then(|| {
            row["source"]
                .as_str()
                .and_then(|value| Repository::parse(value).ok())
        })
        .flatten()
}

pub(super) fn from_record(
    ctx: &Context,
    name: &str,
    row: &Value,
    runner: Runner,
) -> Result<Recipe> {
    let unverified = || {
        Error::attention(
            "native_skills_recipe_required",
            format!("本机的 {name} 记录缺少可核对的 GitHub 提交、技能路径或共享目录。"),
        )
    };
    let repository = source_of(row).ok_or_else(unverified)?;
    repository.verify_url(row["sourceUrl"].as_str().ok_or_else(unverified)?)?;
    let commit = ObjectId::parse(row["commit"].as_str().ok_or_else(unverified)?)?;
    let path = policy::skill_directory(row["skillPath"].as_str().ok_or_else(unverified)?)?;
    let hash = row["skillFolderHash"].as_str().ok_or_else(unverified)?;
    if !matches!(hash.len(), 40 | 64) || !hash.bytes().all(|value| value.is_ascii_hexdigit()) {
        return Err(unverified());
    }
    let payload = disk::payload(ctx, name)?;
    let directory = crate::platform::fs::Directory::open(&payload)?;
    if directory.path != payload {
        return Err(unverified());
    }
    let raw = crate::platform::fs::regular_bytes(&payload.join("SKILL.md"))?;
    let local_name = crate::capability::rules::invocation::skill_name(&raw)?;
    let member = row["skillName"].as_str().unwrap_or(name);
    if local_name != member {
        return Err(unverified());
    }
    let manual = crate::capability::rules::invocation::manual_frontmatter(&raw)?;
    let tracking_ref = (row["ref"] == "HEAD").then(|| "HEAD".to_owned());
    recipe(
        name.into(),
        Source {
            repository: repository.key().into(),
            commit: commit.as_str().into(),
            skill_directory: path,
            member_name: member.into(),
            tracking_ref,
        },
        manual,
        runner,
    )
}

fn recipe(name: String, source: Source, manual_only: bool, runner: Runner) -> Result<Recipe> {
    let identity =
        crate::domain::fingerprint(&(&source.repository, &source.skill_directory, &name))?;
    let recipe = Recipe {
        schema_version: 1,
        id: format!("github-{identity}"),
        owner: "skills-cli".into(),
        cli_version: policy::CLI_VERSION.into(),
        source,
        install_name: name,
        scope: "global".into(),
        client_scope: vec!["codex".into()],
        copy: true,
        manual_only,
        license_files: vec![],
        derived: None,
        runner,
    };
    policy::validate(&recipe)?;
    Ok(recipe)
}

pub(super) fn runner(ctx: &Context) -> Result<Runner> {
    let missing = || {
        Error::attention(
            "skills_cli_unavailable",
            "本机尚未准备好受支持的 Skills CLI (1.9.0) 或 Node.js。",
        )
        .with_guidance(
            "无法准备技能操作。",
            "运行 npx --yes skills@1.9.0 --version 准备工具，然后重试原命令。",
        )
    };
    let accessible = |path: &Path| !ctx.isolated || path.starts_with(&ctx.home);
    let on_path = crate::operations::command::executable("node")
        .ok()
        .filter(|path| accessible(path));
    let mise = ctx
        .scope
        .paths
        .get("MISE_DATA_DIR")
        .and_then(Option::as_ref)
        .cloned()
        .unwrap_or_else(|| ctx.home.join(".local/share/mise"));
    let node = on_path
        .filter(|path| {
            !path
                .parent()
                .is_some_and(|parent| parent.ends_with("mise/shims"))
        })
        .and_then(|path| crate::platform::fs::read_path(&path).ok())
        .or_else(|| {
            crate::platform::fs::read_path(&mise.join("installs/node/latest/bin/node")).ok()
        });
    let node = node
        .filter(|path| accessible(path) && path.is_file())
        .ok_or_else(missing)?;
    let path_entry = crate::operations::command::executable("skills")
        .ok()
        .and_then(|path| crate::platform::fs::read_path(&path).ok())
        .filter(|path| accessible(path));
    if let Some(entry) = path_entry.filter(|path| valid_runner_entry(path)) {
        return Ok(Runner {
            node,
            entry,
            sha256: None,
        });
    }
    let cache = ctx.home.join(".npm/_npx");
    let cache_directory = crate::platform::fs::Directory::open(&cache).map_err(|_| missing())?;
    if cache_directory.path != cache {
        return Err(missing());
    }
    let mut entries = BTreeSet::new();
    for row in std::fs::read_dir(&cache)?.take(256) {
        let row = row?;
        if row.file_type()?.is_dir() {
            let entry = row.path().join("node_modules/skills/bin/cli.mjs");
            if crate::platform::fs::read_path(&entry).is_ok_and(|physical| physical == entry)
                && valid_runner_entry(&entry)
            {
                entries.insert(entry);
            }
        }
    }
    cache_directory.verify()?;
    let entry = entries.into_iter().next().ok_or_else(missing)?;
    Ok(Runner {
        node,
        entry,
        sha256: None,
    })
}

fn valid_runner_entry(entry: &Path) -> bool {
    let Some(package) = entry.parent().and_then(Path::parent) else {
        return false;
    };
    entry.ends_with("skills/bin/cli.mjs")
        && entry.is_file()
        && crate::platform::fs::regular_json::<Value>(&package.join("package.json")).is_ok_and(
            |value| {
                value["name"] == "skills"
                    && value["version"] == policy::CLI_VERSION
                    && value["bin"]["skills"] == "./bin/cli.mjs"
            },
        )
}

#[cfg(test)]
#[path = "../../tests/unit/public_skill.rs"]
mod tests;
