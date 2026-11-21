use crate::{
    error::{Error, Result},
    policy::{
        delivery::{self, Selection, SourceGroup},
        source_git::{Component, ObjectId, Repository},
    },
};
use std::collections::BTreeSet;
#[derive(Clone, Debug)]
pub struct Member {
    pub id: String,
    pub canonical_name: String,
    pub repository: Repository,
    pub aliases: BTreeSet<String>,
}
#[derive(Clone, Debug)]
pub struct Request {
    pub selection: Selection,
    pub commit: Option<ObjectId>,
    pub change_id: Option<String>,
}
impl Request {
    pub fn parse(
        name: Option<&str>,
        only: &[String],
        all: bool,
        revision: Option<&str>,
        change: Option<&str>,
    ) -> Result<Self> {
        let selection = delivery::selection(name, only, all, None, change)?;
        let commit = revision
            .map(|s| crate::policy::skills::revision(s).and_then(|s| ObjectId::parse(&s)))
            .transpose()?;
        Ok(Self {
            selection,
            commit,
            change_id: change.map(str::to_owned),
        })
    }
}
fn invalid() -> Error {
    Error::attention("invalid_source_selection", "来源成员、分组或选择无法验证")
}
fn key(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}
fn validate(members: &[Member], groups: &[SourceGroup]) -> Result<()> {
    if members.len() > 5000 || groups.len() > 5000 {
        return Err(invalid());
    }
    let mut ids = BTreeSet::new();
    for member in members {
        Component::parse(&member.canonical_name)?;
        if !key(&member.id) || !ids.insert(&member.id) || member.aliases.iter().any(|a| !key(a)) {
            return Err(invalid());
        }
    }
    let mut group_ids = BTreeSet::new();
    let mut references = 0;
    for group in groups {
        references += group.members.len();
        if !group.id.starts_with("source:")
            || !key(&group.id)
            || !group_ids.insert(&group.id)
            || group.aliases.iter().any(|a| !key(a))
            || group.members.iter().any(|id| !ids.contains(id))
            || references > 20000
        {
            return Err(invalid());
        }
    }
    Ok(())
}
fn expand(selector: &str, members: &[Member], groups: &[SourceGroup]) -> Result<BTreeSet<String>> {
    let mut matches = vec![];
    let qualified_member = selector.starts_with("asset:") || selector.starts_with("capability:");
    let qualified_group = selector.starts_with("source:");
    for member in members {
        if member.id == selector
            || (!qualified_member
                && !qualified_group
                && (member.canonical_name == selector || member.aliases.contains(selector)))
        {
            matches.push(BTreeSet::from([member.id.clone()]));
        }
    }
    for group in groups {
        if group.id == selector
            || (!qualified_member && !qualified_group && group.aliases.contains(selector))
        {
            matches.push(group.members.clone());
        }
    }
    let [one] = matches.as_slice() else {
        return Err(Error::attention(
            "source_selection_ambiguous_or_missing",
            "来源选择缺失或有歧义，请使用明确成员 ID 或 source:分组",
        ));
    };
    if one.is_empty() {
        return Err(Error::attention(
            "source_selection_empty",
            "所选分组没有可验证的来源成员",
        ));
    }
    Ok(one.clone())
}
pub fn resolve(
    request: &Request,
    members: &[Member],
    groups: &[SourceGroup],
) -> Result<BTreeSet<String>> {
    validate(members, groups)?;
    let mut selected = BTreeSet::new();
    match &request.selection {
        Selection::All => selected.extend(members.iter().map(|m| m.id.clone())),
        Selection::Explicit(keys) => {
            if keys.is_empty() {
                return Err(invalid());
            }
            for selector in keys {
                if !key(selector) {
                    return Err(invalid());
                }
                for member in expand(selector, members, groups)? {
                    if !selected.insert(member) {
                        return Err(Error::attention(
                            "source_selection_overlap",
                            "来源与成员或多个分组重复覆盖同一安装",
                        ));
                    }
                }
            }
        }
    }
    if selected.len() > 128 {
        return Err(invalid());
    }
    let mut targets = BTreeSet::new();
    let mut repositories = BTreeSet::new();
    for member in members.iter().filter(|m| selected.contains(&m.id)) {
        if !targets.insert(&member.canonical_name) {
            return Err(Error::attention(
                "source_target_overlap",
                "多个来源声明指向同一 canonical 名称，请先明确归属",
            ));
        }
        repositories.insert(member.repository.key());
    }
    if request.commit.is_some() && repositories.len() > 1 {
        return Err(Error::attention(
            "source_revision_cross_repository",
            "一次显式提交不能用于不同仓库的批次",
        ));
    }
    Ok(selected)
}
#[cfg(test)]
#[path = "../../tests/unit/source_select_policy.rs"]
mod tests;
