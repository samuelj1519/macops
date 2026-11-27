use crate::{
    browser::{Item, Kind, View},
    context::Context,
    domain::resource::{Management, State},
    error::Result,
    policy::public_source::repository_subject,
    terminal::presentation::{table, table_width},
};
use serde_json::Value;
use std::fmt::Write as _;

const INLINE_ITEMS: usize = 10;

pub(super) struct Related {
    pub views: Vec<(Kind, View)>,
    incomplete: bool,
    repository_query: bool,
}

impl Related {
    pub(super) fn read(ctx: &Context, data: &Value) -> Self {
        let mut matches = Self {
            views: Vec::new(),
            incomplete: false,
            repository_query: false,
        };
        let Some(query) = data["query"].as_str() else {
            return matches;
        };
        let repository = data["repository"]
            .as_str()
            .and_then(repository_subject)
            .or_else(|| repository_subject(query));
        matches.repository_query = repository.is_some();
        let applications = repository
            .is_none()
            .then(|| (Kind::Apps, crate::browser::local_applications(ctx)));
        let capabilities = (
            Kind::Capabilities,
            crate::capability::capabilities::browse(ctx, true),
        );
        for (kind, result) in applications.into_iter().chain([capabilities]) {
            let Ok(mut view) = result else {
                matches.incomplete = true;
                continue;
            };
            view.items.retain(|item| {
                if let Ok(related) = related_item(ctx, item, kind, query, repository) {
                    related
                } else {
                    view.incomplete = true;
                    false
                }
            });
            matches.incomplete |= view.incomplete;
            if !view.items.is_empty() {
                matches.views.push((kind, view));
            }
        }
        matches
    }

    pub(super) fn status(&self) -> String {
        let mut text = String::new();
        if self.views.is_empty() && !self.incomplete {
            text.push_str("  当前盘点未发现可核验的匹配记录。\n");
        }
        for (kind, view) in &self.views {
            let registered = view
                .items
                .iter()
                .filter(|item| item.management == Management::Adopted)
                .count();
            let _ = writeln!(
                text,
                "  {} · {} 项 · 已接管 {registered}",
                title(*kind),
                view.items.len()
            );
            let mut rows = Vec::new();
            for item in view.items.iter().take(INLINE_ITEMS) {
                let Some(info) = item.inventory.as_ref() else {
                    continue;
                };
                let state = match info.status {
                    State::Present => "已发现",
                    State::Missing => "载荷缺失",
                    State::Unknown => "待核验",
                    State::Declared => "已声明",
                };
                let version = info.version.as_deref().unwrap_or("版本未核验");
                let place = if !info.groups.is_empty() {
                    info.groups
                        .iter()
                        .map(|group| crate::terminal::group_label(group))
                        .collect::<Vec<_>>()
                        .join("、")
                } else if item.agents.is_empty() {
                    "—".into()
                } else {
                    item.agents.join("、")
                };
                let basis = if self.repository_query && info.repository.is_none() {
                    "市场登记"
                } else {
                    "—"
                };
                rows.push(vec![
                    if matches!(info.manager.as_str(), "claude-plugin" | "codex-config") {
                        crate::agent::plugin_install::display_name(&item.name).into()
                    } else {
                        item.name.clone()
                    },
                    state.into(),
                    version.into(),
                    place,
                    info.manager.clone(),
                    basis.into(),
                ]);
            }
            let mut headers = vec!["名称", "安装状态", "版本", "安装位置", "渠道"];
            if rows.iter().any(|row| row[5] == "市场登记") {
                headers.push("匹配依据");
            } else {
                for row in &mut rows {
                    row.pop();
                }
            }
            text.push_str(&table(&headers, &rows, table_width()));
            if view.items.len() > INLINE_ITEMS {
                let _ = writeln!(
                    text,
                    "    另有 {} 项，可在维护入口查看。",
                    view.items.len() - INLINE_ITEMS
                );
            }
        }
        if self.incomplete {
            text.push_str("  部分本机记录无法核验。\n");
        }
        text
    }

    pub(super) fn maintenance(&self, first: usize) -> String {
        let mut text = String::new();
        if self.views.is_empty() {
            return "  当前盘点没有可打开的匹配记录。\n".into();
        }
        let mut rows = Vec::new();
        let mut has_plugins = false;
        for (index, (kind, view)) in self.views.iter().enumerate() {
            let action = if *kind == Kind::Apps {
                "查看和维护软件"
            } else {
                "查看技能与插件"
            };
            rows.push(vec![
                (first + index).to_string(),
                action.into(),
                view.items.len().to_string(),
            ]);
            has_plugins |= view.items.iter().any(|item| {
                item.inventory.as_ref().is_some_and(|info| {
                    matches!(info.manager.as_str(), "claude-plugin" | "codex-config")
                })
            });
        }
        text.push_str(&table(&["编号", "入口", "项目数"], &rows, table_width()));
        if has_plugins {
            text.push_str("原生插件在对应 Agent 的插件菜单中维护。\n");
        }
        text
    }
}

fn title(kind: Kind) -> &'static str {
    if kind == Kind::Apps {
        "软件"
    } else {
        "技能与插件"
    }
}

fn related_item(
    ctx: &Context,
    item: &Item,
    kind: Kind,
    query: &str,
    repository: Option<&str>,
) -> Result<bool> {
    let Some(info) = item.inventory.as_ref() else {
        return Ok(false);
    };
    if kind == Kind::Apps && info.status == State::Declared {
        return Ok(false);
    }
    if let Some(repository) = repository {
        if let Some(source) = &info.repository {
            return Ok(source.key().eq_ignore_ascii_case(repository));
        }
        let Some(record) = info.native_record.as_ref() else {
            return Ok(false);
        };
        let source = crate::agent::plugin_marketplaces::registered_github_repository(
            ctx,
            &info.manager,
            record,
            &item.name,
        )?;
        return Ok(source.is_some_and(|source| source.key().eq_ignore_ascii_case(repository)));
    }
    Ok([&item.id, &item.name]
        .into_iter()
        .chain(&info.aliases)
        .any(|key| key.eq_ignore_ascii_case(query)))
}

#[cfg(test)]
#[path = "../../tests/unit/recommend_inventory.rs"]
mod tests;
