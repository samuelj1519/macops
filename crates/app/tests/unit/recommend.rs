use super::*;
use crate::test_support::TestResult;

#[test]
fn recommendation_preserves_manager_and_scope() -> TestResult {
    for (line, expected) in [
        ("brew install jq", Some(("brew", "jq"))),
        ("brew install --formula jq", Some(("brew", "jq"))),
        (
            "brew install --cask visual-studio-code",
            Some(("brew", "visual-studio-code")),
        ),
        (
            "brew install vendor/cli/tool",
            Some(("brew", "vendor/cli/tool")),
        ),
        ("brew install wrong/tap", None),
        ("brew install --cask --unknown", None),
        ("npm install eslint", Some(("npm", "eslint"))),
        ("npm install eslint --save-dev", Some(("npm", "eslint"))),
        ("npm install --save-dev eslint", Some(("npm", "eslint"))),
        ("npm i -g @scope/demo@1.0.0", Some(("npm", "@scope/demo"))),
        (
            "npm install typescript --global",
            Some(("npm", "typescript")),
        ),
        ("npm install --global @scope/demo@^3", None),
        ("npm install @scope@latest", None),
        ("npx tsc", Some(("npx", "tsc"))),
        ("npx --yes skills", None),
        ("mise install node@24", Some(("mise", "node"))),
        ("mise use node@24", Some(("mise", "node"))),
        ("mise use -g node@24", Some(("mise", "node"))),
        ("mise use node@", None),
        ("mise install node", Some(("mise", "node"))),
        ("mise install ../node", None),
        ("mise install node@invalid:selector", None),
        ("npx skills add org/skills", Some(("skills", "org/skills"))),
        (
            "npx skills add org/skills --skill demo --agent codex --copy --global",
            Some(("skills", "org/skills")),
        ),
        ("npx skills add org/skills --agent '*' ", None),
        (
            "npx skills add org/skills --agent codex --agent cursor",
            None,
        ),
        ("npx skills add org/skills --copy --copy", None),
        ("npx skills add org/skills --global --global", None),
        ("npx skills add org/skills --skill demo --skill twice", None),
        (
            "npx skills add org/skills -s demo -a codex",
            Some(("skills", "org/skills")),
        ),
        ("npx skills add org/skills --skill -unsafe", None),
        ("npx skills add org/skills --agent -unsafe", None),
        ("npx skills add missing-owner", None),
        ("npx skills add -owner/name", None),
        ("npx skills add org/name@unfixed", None),
        ("npm install demo | another-command", None),
    ] {
        match (parse(line), expected) {
            (None, None) => (),
            (Some(candidate), Some((channel, key))) => {
                let actual = serde_json::to_value(candidate)?;
                assert_eq!(actual["channel"], channel, "{line}");
                assert_eq!(
                    actual[if channel == "skills" {
                        "repository"
                    } else {
                        "name"
                    }],
                    key,
                    "{line}"
                );
            }
            _ => return Err(format!("wrong candidate for {line}").into()),
        }
    }
    Ok(())
}

#[test]
fn evidence_requires_a_domain_url_and_bounded_related_snippets() -> TestResult {
    for url in [
        "http://example.org",
        "https://user@example.org",
        "https://example.org:22",
        "https://example.org/line\nbreak",
        "https://example.web body",
        "https://localhost",
        "https://127.0.0.1",
        "https://.com",
        "https://example.",
        "https://example..org",
        "https://-example.org",
        "https://example-.org",
        "https://example.c0m",
        "https://sub_domain.example.org",
    ] {
        assert!(!domain_link(url), "{url}");
    }
    assert!(domain_link(
        "https://tools.example.org/install?platform=mac#docs"
    ));
    assert!(!domain_link(&format!(
        "https://example.org/{}",
        "a".repeat(2048)
    )));
    for query in ["", "  ", "a\nb", &"x".repeat(129)] {
        assert!(query_name(query).is_err());
    }
    assert_eq!(query_name(" editor ")?, "editor");
    let source = |subject: &str, excerpt: &str| Evidence {
        url: "https://example.org/download".into(),
        subject: subject.into(),
        excerpt: excerpt.into(),
    };
    let mut bundle = Input {
        schema_version: 1,
        sources: vec![
            source(
                "Editor",
                "\neditor reference\n`brew install editor`\nbrew typo command",
            ),
            source("Editor", "brew install editor"),
            source("Unrelated", "brew install other"),
        ],
    };
    assert!(analyze("", &bundle).is_err());
    let output = analyze("editor", &bundle)?;
    assert_eq!(output.findings.len(), 1);
    assert_eq!(output.findings[0].evidence_urls.len(), 1);
    assert_eq!(output.unrelated_sources, 1);
    assert_eq!(output.unknown_command_examples, 1);

    bundle.sources[0].subject = " ".into();
    assert!(analyze("editor", &bundle).is_err());
    bundle.sources[0] = source("Editor", &"x".repeat(9 * 1024));
    assert!(analyze("editor", &bundle).is_err());
    bundle.sources[0] = source("Editor", &"x\n".repeat(129));
    assert!(analyze("editor", &bundle).is_err());
    bundle.sources[0].url = "http://example.org".into();
    assert!(analyze("editor", &bundle).is_err());
    bundle.sources.clear();
    assert!(analyze("editor", &bundle).is_err());
    bundle.sources = (0..17)
        .map(|_| source("Editor", "brew install editor"))
        .collect();
    assert!(analyze("editor", &bundle).is_err());
    bundle.schema_version = 2;
    assert!(analyze("editor", &bundle).is_err());
    Ok(())
}

#[test]
fn verified_public_install_examples_are_a_small_auditable_snapshot() -> TestResult {
    for (product, candidates) in [
        ("jq", 1),
        ("TypeScript", 3),
        ("node", 2),
        ("vercel-labs/agent-skills", 2),
    ] {
        let source = public_snapshot(product).ok_or("missing public snapshot")?;
        let result = analyze(product, &source)?;
        assert_eq!(result.findings.len(), candidates);
        assert_eq!(result.unknown_command_examples, 0);
    }
    assert!(public_snapshot("private-package").is_none());
    Ok(())
}
