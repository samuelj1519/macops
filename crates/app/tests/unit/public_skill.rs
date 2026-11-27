#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::{
    fs,
    io::Write as _,
    path::PathBuf,
    process::{Command, Stdio},
};

struct Repo {
    _dir: tempfile::TempDir,
    git: Git,
    objects: PathBuf,
    ctx: Context,
    runner: Runner,
}

impl Repo {
    fn new(request: &Request) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir_in("/private/tmp")?;
        let git_dir = dir.path().join("git");
        let git = Git::create_bound(&git_dir, &request.repository)?;
        let objects = git_dir.join("objects.git");
        let home = dir.path().join("home");
        fs::create_dir(&home)?;
        let cli = Cli::try_parse_from([
            "mo",
            "--home",
            home.to_str().ok_or("fixture home")?,
            "skill",
        ])?;
        let ctx = Context::from_cli(&cli, &mut vec![])?;
        let node = home.join(".local/share/mise/installs/node/latest/bin/node");
        fs::create_dir_all(node.parent().ok_or("node directory")?)?;
        fs::copy("/usr/bin/true", &node)?;
        let package = home.join(".npm/_npx/fixture/node_modules/skills");
        let entry = package.join("bin/cli.mjs");
        fs::create_dir_all(entry.parent().ok_or("runner directory")?)?;
        fs::write(&entry, "// test")?;
        fs::write(
            package.join("package.json"),
            br#"{"name":"skills","version":"1.9.0","bin":{"skills":"./bin/cli.mjs"}}"#,
        )?;
        let runner = runner(&ctx)?;
        assert_eq!(runner.node, node);
        assert_eq!(runner.entry, entry);
        Ok(Self {
            _dir: dir,
            git,
            objects,
            ctx,
            runner,
        })
    }

    fn object(
        &self,
        cmd: &[&str],
        data: &str,
    ) -> std::result::Result<ObjectId, Box<dyn std::error::Error>> {
        let mut child = Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.ctx.home)
            .arg("--git-dir")
            .arg(&self.objects)
            .args(cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or("fixture stdin")?
            .write_all(data.as_bytes())?;
        let out = child.wait_with_output()?;
        assert!(out.status.success());
        Ok(ObjectId::parse(std::str::from_utf8(&out.stdout)?.trim())?)
    }

    fn blob(&self, data: &str) -> std::result::Result<ObjectId, Box<dyn std::error::Error>> {
        self.object(&["hash-object", "-w", "--stdin"], data)
    }

    fn tree(&self, data: &str) -> std::result::Result<ObjectId, Box<dyn std::error::Error>> {
        self.object(&["mktree"], data)
    }

    fn skill_tree(
        &self,
        name: &str,
        manual: bool,
    ) -> std::result::Result<ObjectId, Box<dyn std::error::Error>> {
        let blob = self.blob(&format!(
            "---\nname: {name}\ndisable-model-invocation: {manual}\n---\nBody\n"
        ))?;
        self.tree(&format!("100644 blob {}\tSKILL.md\n", blob.as_str()))
    }
}

#[test]
fn fixed_repository_tree_discovers_root_and_nested_skills_and_plugin_evidence() -> TestResult {
    let request = Request::parse("https://github.com/owner/repo@v2.0.0")?;
    let f = Repo::new(&request)?;
    let root_skill = f.blob("---\nname: root-example\n---\nBody\n")?;
    let member = f.skill_tree("nested-example", true)?;
    let empty = f.tree("")?;
    let folders = f.tree(&format!(
        "040000 tree {}\talpha\n040000 tree {}\tignored\n100644 blob {}\tREADME.md\n",
        member.as_str(),
        empty.as_str(),
        root_skill.as_str()
    ))?;
    let agents = f.tree(&format!("040000 tree {}\tskills\n", folders.as_str()))?;
    let market =
        f.blob(r#"{"name":"fixture-market","plugins":[{"name":"widget","source":"./"}]}"#)?;
    let widget = f.blob(r#"{"name":"widget","version":"2.0.0"}"#)?;
    let plugin = f.tree(&format!(
        "100644 blob {}\tmarketplace.json\n100644 blob {}\tplugin.json\n",
        market.as_str(),
        widget.as_str()
    ))?;
    let tree = f.tree(&format!(
        "100644 blob {}\tSKILL.md\n040000 tree {}\t.agents\n040000 tree {}\t.claude-plugin\n",
        root_skill.as_str(),
        agents.as_str(),
        plugin.as_str()
    ))?;
    let commit = ObjectId::parse(&"a".repeat(40))?;
    let recipes = request.skills_from_tree(&f.git, &tree, &commit, &f.runner)?;
    assert_eq!(recipes.len(), 2);
    assert_eq!(recipes[0].install_name, "root-example");
    assert_eq!(recipes[1].source.skill_directory, ".agents/skills/alpha");
    assert!(recipes[1].manual_only);
    assert!(
        recipes
            .iter()
            .all(|recipe| recipe.source.commit == commit.as_str()
                && recipe.source.tracking_ref.is_none())
    );
    let offer = request
        .claude_offer_from_tree(&f.git, &tree, &commit)?
        .ok_or("plugin offer")?;
    assert_eq!(offer.version, "2.0.0");
    let main = Request::parse("github.com/owner/repo")?;
    assert_eq!(main.tracking_ref.as_deref(), Some("HEAD"));
    assert!(Request::parse("https://example.org/owner/repo").is_err());
    Ok(())
}

#[test]
fn public_repository_empty_duplicate_and_oversized_catalogs_are_rejected() -> TestResult {
    use std::fmt::Write as _;
    let req = Request::parse("owner/repo")?;
    let f = Repo::new(&req)?;
    let empty = f.tree("")?;
    let commit = ObjectId::parse(&"a".repeat(40))?;
    assert!(
        req.claude_offer_from_tree(&f.git, &empty, &commit)?
            .is_none()
    );
    let select = |tree: &ObjectId| req.skills_from_tree(&f.git, tree, &commit, &f.runner);
    assert_eq!(
        select(&empty).err().ok_or("empty tree")?.code,
        "public_skill_members_unavailable"
    );

    let skill = f.skill_tree("same-name", false)?;
    let skills = f.tree(&format!(
        "040000 tree {}\tone\n040000 tree {}\ttwo\n",
        skill.as_str(),
        skill.as_str()
    ))?;
    let dupe = f.tree(&format!("040000 tree {}\tskills\n", skills.as_str()))?;
    assert_eq!(
        select(&dupe).err().ok_or("duplicate name")?.code,
        "public_skill_members_unavailable"
    );
    let mut rows = String::new();
    for n in 0..128 {
        writeln!(rows, "040000 tree {}\tmember-{n:03}", empty.as_str())?;
    }
    let skills = f.tree(&rows)?;
    let oversized = f.tree(&format!("040000 tree {}\tskills\n", skills.as_str()))?;
    assert_eq!(
        select(&oversized).err().ok_or("limit exceeded")?.code,
        "public_skill_member_limit"
    );
    fs::remove_file(f.runner.entry)?;
    assert!(runner(&f.ctx).is_err());
    Ok(())
}
