use crate::{
    browser::{Item, View},
    context::Context,
    error::{Error, Result},
    native_plugins::valid_name,
    native_plugins_files::{Passive, object_file},
    resource::{Data, Management},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
};

const CODEX_CATALOGS: [&str; 4] = [
    ".agents/plugins/marketplace.json",
    ".agents/plugins/api_marketplace.json",
    ".claude-plugin/marketplace.json",
    ".cursor-plugin/marketplace.json",
];

#[derive(Debug, Serialize)]
struct Market {
    name: String,
    scope: String,
    source_kind: String,
    repository: Option<String>,
    plugins: Option<Vec<String>>,
    issue: Option<&'static str>,
}

#[derive(Clone, Copy)]
enum Harness {
    Codex,
    Claude,
    Cursor,
}

impl Harness {
    const ALL: [Self; 3] = [Self::Codex, Self::Claude, Self::Cursor];

    fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Cursor => "cursor",
        }
    }

    fn official(self) -> &'static str {
        match self {
            Self::Codex => "https://developers.openai.com/codex/plugins",
            Self::Claude => "https://claude.com/marketplace",
            Self::Cursor => "https://cursor.com/marketplace",
        }
    }

    fn native_entry(self) -> &'static str {
        match self {
            Self::Codex => "/plugins",
            Self::Claude | Self::Cursor => "/plugin",
        }
    }
}

fn profile_roots(
    ctx: &Context,
    selector: Option<&str>,
) -> Result<BTreeMap<String, Option<PathBuf>>> {
    let mut roots = BTreeMap::new();
    for harness in Harness::ALL {
        let client = harness.id();
        roots.insert(
            format!("{client}:default"),
            Some(ctx.home.join(format!(".{client}"))),
        );
    }
    let include = |client: &str, profile: &str| {
        matches!(client, "codex" | "claude")
            && valid_name(profile)
            && selector.is_none_or(|id| id == client || id == format!("{client}:{profile}"))
    };
    for profile in crate::resource_agents::profile_roots_matching(ctx, include)? {
        if let Data::Agent { config_dir, .. } = profile.data {
            roots
                .entry(profile.name)
                .and_modify(|root| {
                    if root.as_ref() != Some(&config_dir) {
                        *root = None;
                    }
                })
                .or_insert(Some(config_dir));
        }
    }
    Ok(roots)
}

pub(crate) fn cursor_profile(selector: Option<&str>) -> bool {
    matches!(selector, Some("cursor" | "cursor:default"))
}

pub(crate) fn list(
    ctx: &Context,
    selector: Option<&str>,
    online: bool,
    dry_run: bool,
) -> Result<Value> {
    if online && !cursor_profile(selector) {
        return Err(Error::new(
            "marketplace_online_unsupported",
            "--online 只用于 Cursor 账户市场",
        ));
    }
    let roots = profile_roots(ctx, selector)?;
    let mut profiles = Vec::new();
    let mut incomplete = false;
    for harness in Harness::ALL {
        for (profile, root) in roots.iter().filter(|(profile, _)| {
            profile.starts_with(&format!("{}:", harness.id()))
                && selector.is_none_or(|id| id == harness.id() || id == *profile)
        }) {
            let (status, markets, reason, partial) = match (harness, root.as_deref()) {
                (_, None) => (
                    "unknown",
                    vec![],
                    Some("marketplace_profile_ambiguous".into()),
                    true,
                ),
                (Harness::Cursor, _) if !online => ("account_query_available", vec![], None, false),
                (Harness::Cursor, _) if dry_run => ("dry_run", vec![], None, false),
                (Harness::Cursor, _) => observe(cursor_markets(ctx), "account"),
                (Harness::Codex, Some(root)) => observe(codex_markets(root), "local"),
                (Harness::Claude, Some(root)) => observe(claude_markets(root), "local"),
            };
            incomplete |= partial;
            profiles.push(json!({
                "harness": harness.id(), "profile": profile,
                "official": harness.official(), "nativeEntry": harness.native_entry(),
                "status": status, "reason": reason, "marketplaces": markets
            }));
        }
    }
    if profiles.is_empty() {
        return Err(Error::attention(
            "marketplace_profile_unknown",
            "没有找到该 Harness 档案",
        ));
    }
    Ok(json!({
        "kind": "plugin-marketplaces", "profiles": profiles, "incomplete": incomplete,
        "macopsWrites": false, "nativeQueryRequested": online && !dry_run
    }))
}

fn observe(
    result: Result<Vec<Market>>,
    source: &'static str,
) -> (&'static str, Vec<Market>, Option<String>, bool) {
    match result {
        Ok(markets) => {
            let partial = markets.iter().any(|market| market.issue.is_some());
            (source, markets, None, partial)
        }
        Err(error) => ("unknown", vec![], Some(error.code), true),
    }
}

fn invalid() -> Error {
    Error::attention("marketplace_metadata_unknown", "无法核对原生市场元数据")
}

pub(crate) fn public_repository(value: &str) -> Option<String> {
    let repo = value.strip_prefix("https://github.com/").unwrap_or(value);
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let (owner, name) = repo.split_once('/')?;
    (valid_name(owner) && valid_name(name)).then(|| format!("https://github.com/{repo}"))
}

fn codex_markets(root: &Path) -> Result<Vec<Market>> {
    let mut reader = Passive::new()?;
    let Some(bytes) = reader.file(&root.join("config.toml"))? else {
        reader.verify()?;
        return Ok(vec![]);
    };
    let config = std::str::from_utf8(&bytes)
        .map_err(|_| invalid())?
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| invalid())?;
    let mut markets = Vec::new();
    if let Some(entries) = config.get("marketplaces") {
        let entries = entries.as_table_like().ok_or_else(invalid)?;
        for (name, item) in entries.iter() {
            let entry = item.as_table_like().ok_or_else(invalid)?;
            let field = |key: &str| entry.get(key).and_then(toml_edit::Item::as_str);
            let source = field("source").ok_or_else(invalid)?;
            let kind = field("source_type").ok_or_else(invalid)?;
            let path = match kind {
                "local" => Some(PathBuf::from(source)),
                "git" if valid_name(name) => Some(root.join(".tmp/marketplaces").join(name)),
                _ => None,
            };
            markets.push(local_market(
                name,
                kind,
                source,
                path.as_deref(),
                &CODEX_CATALOGS,
            )?);
        }
    }
    reader.verify()?;
    Ok(markets)
}

fn claude_markets(root: &Path) -> Result<Vec<Market>> {
    let mut reader = Passive::new()?;
    let book = object_file(&mut reader, &root.join("plugins/known_marketplaces.json"))?;
    let mut markets = Vec::new();
    if let Some(book) = &book {
        for (name, entry) in book.as_object().ok_or_else(invalid)? {
            let source = entry
                .get("source")
                .and_then(Value::as_object)
                .ok_or_else(invalid)?;
            let kind = source
                .get("source")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            let repository = source.get("repo").or_else(|| source.get("url"));
            let install = entry["installLocation"].as_str().map(Path::new);
            markets.push(local_market(
                name,
                kind,
                repository.and_then(Value::as_str).unwrap_or(""),
                install,
                &[".claude-plugin/marketplace.json"],
            )?);
        }
    }
    reader.verify()?;
    Ok(markets)
}

fn local_market(
    name: &str,
    kind: &str,
    source: &str,
    path: Option<&Path>,
    locations: &[&str],
) -> Result<Market> {
    if !valid_name(name) || !valid_name(kind) {
        return Err(invalid());
    }
    let catalog = path
        .map(|path| catalog_names(path, name, locations))
        .transpose();
    let (plugins, issue) = match catalog {
        Ok(plugins) => (plugins.flatten(), None),
        Err(_) => (None, Some("catalog_unknown")),
    };
    Ok(Market {
        name: name.into(),
        scope: "profile".into(),
        source_kind: kind.into(),
        repository: matches!(kind, "git" | "github" | "url")
            .then(|| public_repository(source))
            .flatten(),
        plugins,
        issue,
    })
}

fn catalog_names(root: &Path, expected: &str, locations: &[&str]) -> Result<Option<Vec<String>>> {
    let clean = crate::policy::paths::lexical(root)?;
    if clean != root || root.parent().is_none() {
        return Err(invalid());
    }
    let mut reader = Passive::new()?;
    for location in locations {
        if let Some(manifest) = object_file(&mut reader, &root.join(location))? {
            let plugins = manifest["plugins"].as_array().ok_or_else(invalid)?;
            if manifest["name"].as_str() != Some(expected) || plugins.len() > 512 {
                return Err(invalid());
            }
            let mut names = Vec::with_capacity(plugins.len());
            for plugin in plugins {
                let name = plugin["name"]
                    .as_str()
                    .filter(|name| valid_name(name))
                    .ok_or_else(invalid)?;
                names.push(name.to_owned());
            }
            names.sort();
            if names.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(invalid());
            }
            reader.verify()?;
            return Ok(Some(names));
        }
    }
    reader.verify()?;
    Ok(None)
}

fn cursor_markets(ctx: &Context) -> Result<Vec<Market>> {
    let program = crate::resource_inventory::native_program(ctx, "cursor-agent")?;
    let spec = crate::domain::CommandSpec {
        program,
        args: ["plugin", "marketplace", "list", "--format", "json"]
            .map(str::to_owned)
            .to_vec(),
    };
    let output = crate::resource_inventory::native_query(ctx, &spec, &ctx.home, None)?;
    if output.uncertain || output.code != Some(0) {
        let code = if output.stderr.contains("Authentication required") {
            "cursor_marketplace_login_required"
        } else {
            "cursor_marketplace_query_failed"
        };
        return Err(Error::attention(code, "Cursor 未返回可核对的账户市场"));
    }
    let response = crate::policy::json::parse(output.stdout.as_bytes())?;
    let rows = response
        .as_array()
        .or_else(|| response["marketplaces"].as_array())
        .ok_or_else(invalid)?;
    rows.iter()
        .map(|row| {
            let name = row["name"]
                .as_str()
                .filter(|value| valid_name(value))
                .ok_or_else(invalid)?;
            let scope = row["scope"]
                .as_str()
                .filter(|value| valid_name(value))
                .ok_or_else(invalid)?;
            let repository = row
                .get("gitUrl")
                .or_else(|| row.get("git_url"))
                .and_then(Value::as_str)
                .and_then(public_repository);
            Ok(Market {
                name: name.into(),
                scope: scope.into(),
                source_kind: "cursor-account".into(),
                repository,
                plugins: None,
                issue: None,
            })
        })
        .collect()
}

fn field<'a>(row: &'a Value, name: &str) -> &'a str {
    row[name].as_str().unwrap_or_default()
}

fn profile_summary(profile: &Value) -> String {
    if let Some(reason) = profile["reason"].as_str() {
        return format!("市场状态未知  {reason}");
    }
    match field(profile, "status") {
        "account_query_available" => "查询账户市场  mo skill market cursor --online",
        "dry_run" => "预演，未查询账户市场",
        "account" => "该账户未返回市场",
        _ => "未发现已登记的市场",
    }
    .into()
}

pub(crate) fn view(data: &Value) -> View {
    let mut items = Vec::new();
    for profile in data["profiles"].as_array().into_iter().flatten() {
        if let Some(markets) = profile["marketplaces"]
            .as_array()
            .filter(|rows| !rows.is_empty())
        {
            items.extend(
                markets
                    .iter()
                    .enumerate()
                    .map(|(index, row)| market_item(profile, Some(row), index)),
            );
        } else {
            items.push(market_item(profile, None, 0));
        }
    }
    let incomplete = data["incomplete"] == true;
    View {
        scope: "global".into(),
        items,
        coverage: if incomplete { "partial" } else { "observed" },
        difference_coverage: "not_applicable",
        observed_at: Some(crate::store::now()),
        note: if incomplete {
            "部分市场无法核验"
        } else {
            "目录条目 · 安装状态见能力清单"
        }
        .into(),
        incomplete,
    }
}

fn market_item(profile: &Value, market: Option<&Value>, index: usize) -> Item {
    let id = field(profile, "profile");
    let mut details = vec![
        format!("档案 {id}"),
        format!("原生命令 {}", field(profile, "nativeEntry")),
        format!("官方入口 {}", field(profile, "official")),
    ];
    let (name, summary) = if let Some(row) = market {
        details.push(format!(
            "范围 {} · 来源 {}",
            field(row, "scope"),
            field(row, "source_kind")
        ));
        if let Some(repository) = row["repository"].as_str() {
            details.push(format!("仓库 {repository}"));
        }
        let status = if let Some(plugins) = row["plugins"].as_array() {
            format!("{} 个目录插件", plugins.len())
        } else if row["issue"].is_string() {
            "目录状态未知".into()
        } else {
            "未提供插件明细".into()
        };
        details.push(status.clone());
        details.extend(
            row["plugins"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
        (field(row, "name"), format!("{id}  ·  {status}"))
    } else {
        let status = profile_summary(profile);
        details.push(status.clone());
        (id, status)
    };
    Item {
        id: format!("{id}:{index}"),
        name: name.into(),
        summary,
        managed: false,
        management: Management::Untracked,
        directory: false,
        path: None,
        source: None,
        agents: vec![],
        loaded: None,
        details,
    }
}

pub(crate) fn human(data: &Value) -> String {
    let mut text = String::from("插件市场\n");
    for profile in data["profiles"].as_array().into_iter().flatten() {
        let id = profile["profile"].as_str().unwrap_or_default();
        let _ = writeln!(
            text,
            "\n{id}  ·  {}\n  {}",
            profile["nativeEntry"].as_str().unwrap_or_default(),
            profile["official"].as_str().unwrap_or_default(),
        );
        if profile["marketplaces"].as_array().is_none_or(Vec::is_empty) {
            let _ = writeln!(text, "  {}", profile_summary(profile));
        }
        for row in profile["marketplaces"].as_array().into_iter().flatten() {
            let _ = writeln!(
                text,
                "  {}  ·  {}",
                row["name"].as_str().unwrap_or_default(),
                row["repository"]
                    .as_str()
                    .or_else(|| row["source_kind"].as_str())
                    .unwrap_or_default(),
            );
            if let Some(plugins) = row["plugins"]
                .as_array()
                .filter(|plugins| !plugins.is_empty())
            {
                let names = plugins.iter().filter_map(Value::as_str).collect::<Vec<_>>();
                let _ = writeln!(text, "    {}", names.join(", "));
            }
            if row["issue"].is_string() {
                text.push_str("    目录状态未知\n");
            }
        }
    }
    text
}
