#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::fs;

#[test]
fn readonly_policy_assessment_finds_missing_metadata_without_creating_files() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir_all(&home)?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or("utf8")?,
        "skill",
        "policy",
        "check",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let declaration = ctx.settings.join("manifests/agent-invocation-policy.json");
    fs::create_dir_all(declaration.parent().ok_or("parent")?)?;
    fs::write(
        declaration,
        br#"{"schema_version":1,"standalone_manual_only":["example","absent"]}"#,
    )?;
    let skill = home.join(".agents/skills/example/SKILL.md");
    fs::create_dir_all(skill.parent().ok_or("parent")?)?;
    fs::write(&skill, "---\nname: example\n---\nbody\n")?;
    let authority = crate::operations::governance::capture(&ctx)?;
    let plan = assess(&ctx, &authority)?;
    assert_eq!(plan.patches.len(), 2);
    assert_eq!(plan.missing, vec!["absent"]);
    assert_eq!(plan.summary()["status"], "drift");
    assert!(!ctx.state.exists());
    assert!(!ctx.cache.exists());
    assert!(!skill.parent().ok_or("parent")?.join("agents").exists());
    Ok(())
}

struct PolicyFixture {
    _temp: tempfile::TempDir,
    ctx: Context,
}
impl PolicyFixture {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir_in("/private/tmp")?;
        let home = temp.path().join("home");
        fs::create_dir_all(&home)?;
        let cli = Cli::try_parse_from([
            "macops",
            "--home",
            home.to_str().ok_or("utf8")?,
            "skill",
            "policy",
            "check",
        ])?;
        let ctx = Context::from_cli(&cli, &mut vec![])?;
        Ok(Self { _temp: temp, ctx })
    }
    fn rules(&self, value: &Value) -> Result<()> {
        let path = self
            .ctx
            .settings
            .join("manifests/agent-invocation-policy.json");
        fs::create_dir_all(path.parent().ok_or_else(stale)?)?;
        fs::write(path, serde_json::to_vec(value)?)?;
        Ok(())
    }
    fn skill(&self, name: &str, manual: bool) -> Result<PathBuf> {
        let path = self
            .ctx
            .home
            .join(".agents/skills")
            .join(name)
            .join("SKILL.md");
        fs::create_dir_all(path.parent().ok_or_else(stale)?)?;
        fs::write(
            &path,
            format!("---\nname: {name}\ndisable-model-invocation: {manual}\n---\nbody\n"),
        )?;
        Ok(path)
    }
    fn assess(&self) -> Result<Assessment> {
        assess(
            &self.ctx,
            &crate::operations::governance::capture(&self.ctx)?,
        )
    }
    fn book(&self, book: &OverlayRecord) -> Result<()> {
        let path = self.ctx.home.join(".agents/invocation-overlays.json");
        fs::create_dir_all(path.parent().ok_or_else(stale)?)?;
        fs::write(path, serde_json::to_vec(book)?)?;
        Ok(())
    }
}
fn overlay(original: &[u8], kind: MetadataKind) -> Result<OverlayRow> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let after = invocation::transform(kind, Some(original))?;
    Ok(OverlayRow {
        kind,
        label: "example".into(),
        owner: "source".into(),
        original: Some(STANDARD.encode(original)),
        original_hash: Some(invocation::hash(original)),
        applied_hash: invocation::hash(&after),
        original_mode: Some(0o644),
        applied_mode: 0o644,
    })
}

#[test]
fn mirror_uses_the_original_baseline_and_retiring_overlays_never_resurrects_files() -> TestResult {
    let f = PolicyFixture::new()?;
    let path = f.skill("example", false)?;
    let original = fs::read(&path)?;
    let row = overlay(&original, MetadataKind::Frontmatter)?;
    let (_, applied) = row.bytes()?;
    fs::write(&path, &applied)?;
    let book = OverlayRecord {
        schema_version: 1,
        profile_policy_hash: None,
        profiles: BTreeMap::new(),
        policy_hash: None,
        files: BTreeMap::from([(path.clone(), row)]),
    };
    f.book(&book)?;
    f.rules(&json!({"schema_version":1,"standalone_manual_only":[],"mirror_standalone_upstream_manual":true}))?;
    let retired = f.assess()?;
    assert_eq!(retired.patches.len(), 1);
    assert_eq!(
        retired.patches[0].after.as_deref(),
        Some(original.as_slice())
    );
    assert!(!retired.patches[0].target.path.ends_with("openai.yaml"));
    fs::remove_file(&path)?;
    let gone = f.assess()?;
    assert!(gone.patches.is_empty());
    let after = gone.record_after.ok_or("record")?;
    assert!(invocation::record(&f.ctx.home, &after)?.files.is_empty());
    assert!(!path.exists());
    Ok(())
}

#[test]
fn mirror_adds_codex_metadata_for_upstream_manual_skills_and_detects_new_directory_members()
-> TestResult {
    let f = PolicyFixture::new()?;
    f.skill("upstream", true)?;
    f.skill("devnorm-keep", true)?;
    f.rules(&json!({"schema_version":1,"standalone_manual_only":[],"mirror_standalone_upstream_manual":true}))?;
    let plan = f.assess()?;
    assert_eq!(plan.patches.len(), 1);
    assert!(
        plan.patches[0]
            .target
            .path
            .ends_with("upstream/agents/openai.yaml")
    );
    assert!(
        !plan
            .patches
            .iter()
            .any(|p| p.target.path.to_string_lossy().contains("devnorm"))
    );
    f.skill("new-member", true)?;
    assert!(plan.verify().is_err());
    Ok(())
}

#[test]
fn registered_claude_and_codex_plugin_versions_are_assessed_but_cursor_is_untouched() -> TestResult
{
    let f = PolicyFixture::new()?;
    f.rules(&json!({"schema_version":1,"standalone_manual_only":[],"plugins":{"pstack":{"manual_only":["review"]}}}))?;
    let claude = f.ctx.home.join(".claude-glm");
    let root = claude.join("plugins/cache/market/pstack/1.0.0");
    fs::create_dir_all(root.join("skills/review"))?;
    fs::write(
        root.join("skills/review/SKILL.md"),
        "---\nname: review\n---\nbody\n",
    )?;
    fs::write(
        claude.join("plugins/installed_plugins.json"),
        serde_json::to_vec(
            &json!({"version":2,"plugins":{"pstack@market":[{"scope":"user","installPath":root,"version":"1.0.0"}]}}),
        )?,
    )?;
    let codex = f.ctx.home.join(".codex");
    fs::create_dir_all(&codex)?;
    fs::write(
        codex.join("config.toml"),
        "[plugins.\"pstack@market\"]\nenabled = true\n",
    )?;
    for version in ["1.0.0", "2.0.0"] {
        let skill = codex
            .join("plugins/cache/market/pstack")
            .join(version)
            .join("skills/review");
        fs::create_dir_all(&skill)?;
        fs::write(skill.join("SKILL.md"), "---\nname: review\n---\nbody\n")?;
    }
    let cursor = f
        .ctx
        .home
        .join(".cursor/plugins/cache/market/pstack/1.0.0/skills/review/SKILL.md");
    fs::create_dir_all(cursor.parent().ok_or("parent")?)?;
    fs::write(&cursor, "cursor-owned")?;
    let plan = f.assess()?;
    assert_eq!(plan.patches.len(), 5);
    assert!(
        !plan
            .patches
            .iter()
            .any(|patch| patch.target.path.starts_with(f.ctx.home.join(".cursor")))
    );
    assert_eq!(fs::read_to_string(cursor)?, "cursor-owned");
    let changed = plan.patches[0].target.path.clone();
    fs::write(changed, "external")?;
    assert!(plan.verify().is_err());
    Ok(())
}

#[test]
fn controls_external_edits_and_unsupported_file_types_block_assessment_without_writes() -> TestResult
{
    let f = PolicyFixture::new()?;
    let skill = f.skill("example", false)?;
    f.rules(&json!({"schema_version":1,"standalone_manual_only":["example"]}))?;
    let plan = f.assess()?;
    f.rules(&json!({"schema_version":1,"standalone_manual_only":[]}))?;
    assert!(plan.verify().is_err());
    let original = fs::read(&skill)?;
    let row = overlay(&original, MetadataKind::Frontmatter)?;
    let book = OverlayRecord {
        schema_version: 1,
        profile_policy_hash: None,
        profiles: BTreeMap::new(),
        policy_hash: None,
        files: BTreeMap::from([(skill.clone(), row)]),
    };
    f.book(&book)?;
    fs::write(&skill, "external edit")?;
    assert_eq!(
        f.assess().err().ok_or("drift")?.code,
        "invocation_overlay_drift"
    );
    fs::remove_file(
        f.ctx
            .settings
            .join("manifests/agent-invocation-policy.json"),
    )?;
    assert_eq!(
        f.assess().err().ok_or("declaration")?.code,
        "invocation_declaration_missing"
    );
    assert!(!f.ctx.state.exists());
    Ok(())
}
