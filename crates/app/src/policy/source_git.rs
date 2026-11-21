use crate::error::{Error, Result};
use std::collections::BTreeSet;

fn invalid() -> Error {
    Error::attention("unverified_git_object", "Git 来源、对象或树条目无法验证")
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObjectId(String);
impl ObjectId {
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() != 40
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || value.bytes().all(|b| b == b'0')
        {
            return Err(invalid());
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Repository(String);
impl Repository {
    pub fn parse(value: &str) -> Result<Self> {
        let parts: Vec<_> = value.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|part| {
                part.is_empty()
                    || part.len() > 100
                    || matches!(*part, "." | "..")
                    || !part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        {
            return Err(invalid());
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
    pub fn key(&self) -> &str {
        &self.0
    }
    pub fn url(&self) -> String {
        format!("https://github.com/{}.git", self.0)
    }
    pub fn verify_url(&self, value: &str) -> Result<()> {
        let rest = value
            .strip_prefix("https://github.com/")
            .ok_or_else(invalid)?;
        let parts: Vec<_> = rest.trim_end_matches('/').split('/').collect();
        if parts.len() < 2 {
            return Err(invalid());
        }
        let repo = parts[1].strip_suffix(".git").unwrap_or(parts[1]);
        let actual = Self::parse(&format!("{}/{}", parts[0], repo))?;
        if &actual != self || (parts.len() > 2 && parts[2] != "tree") {
            return Err(invalid());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Component(String);
impl Component {
    pub fn parse(value: &str) -> Result<Self> {
        if value.is_empty()
            || value.len() > 255
            || matches!(value, "." | "..")
            || value.eq_ignore_ascii_case(".git")
            || value
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\'))
        {
            return Err(invalid());
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelativePath(Vec<Component>);
impl RelativePath {
    pub fn parse(value: &str) -> Result<Self> {
        if value == "." {
            return Ok(Self(vec![]));
        }
        let parts: Vec<_> = value.split('/').collect();
        if parts.len() > 32 {
            return Err(invalid());
        }
        Ok(Self(
            parts
                .into_iter()
                .map(Component::parse)
                .collect::<Result<_>>()?,
        ))
    }
    pub fn parts(&self) -> &[Component] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeKind {
    Directory,
    File { executable: bool },
    Symlink,
    Submodule,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: Component,
    pub object: ObjectId,
    pub kind: TreeKind,
}

pub fn tree_entries(raw: &[u8]) -> Result<Vec<TreeEntry>> {
    if raw.len() > 16 * 1024 * 1024 || (!raw.is_empty() && !raw.ends_with(&[0])) {
        return Err(invalid());
    }
    if raw.is_empty() {
        return Ok(vec![]);
    }
    let mut entries = vec![];
    let mut names = BTreeSet::new();
    for record in raw[..raw.len() - 1].split(|byte| *byte == 0) {
        if entries.len() == 10000 {
            return Err(invalid());
        }
        let split = record
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(invalid)?;
        let fields = std::str::from_utf8(&record[..split]).map_err(|_| invalid())?;
        let fields: Vec<_> = fields.split(' ').collect();
        let [mode, kind, object] = fields.as_slice() else {
            return Err(invalid());
        };
        let kind = match (*mode, *kind) {
            ("040000", "tree") => TreeKind::Directory,
            ("100644", "blob") => TreeKind::File { executable: false },
            ("100755", "blob") => TreeKind::File { executable: true },
            ("120000", "blob") => TreeKind::Symlink,
            ("160000", "commit") => TreeKind::Submodule,
            _ => return Err(invalid()),
        };
        let name =
            Component::parse(std::str::from_utf8(&record[split + 1..]).map_err(|_| invalid())?)?;
        if !names.insert(name.clone()) {
            return Err(invalid());
        }
        entries.push(TreeEntry {
            name,
            object: ObjectId::parse(object)?,
            kind,
        });
    }
    Ok(entries)
}

#[cfg(test)]
#[path = "../../tests/unit/source_git_policy.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefName(String);
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reference {
    Head,
    Named(RefName),
    Commit(ObjectId),
}
impl Reference {
    pub fn parse(value: Option<&str>) -> Result<Self> {
        let Some(value) = value else {
            return Ok(Self::Head);
        };
        if value == "HEAD" {
            return Ok(Self::Head);
        }
        if value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(Self::Commit(ObjectId::parse(&value.to_ascii_lowercase())?));
        }
        if value.is_empty()
            || value.len() > 1024
            || value == "@"
            || value.starts_with('-')
            || value.starts_with('/')
            || value.ends_with('/')
            || value.contains("..")
            || value.contains("@{")
            || value.split('/').any(|p| {
                p.is_empty()
                    || p.starts_with('.')
                    || p.ends_with('.')
                    || p.rsplit_once('.')
                        .is_some_and(|(_, suffix)| suffix == "lock")
            })
            || value
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
        {
            return Err(invalid());
        }
        Ok(Self::Named(RefName(value.into())))
    }
    pub fn patterns(&self) -> Vec<String> {
        match self {
            Self::Head => vec!["HEAD".into()],
            Self::Commit(id) => vec![id.as_str().into()],
            Self::Named(RefName(name)) => {
                if name.starts_with("refs/") {
                    let mut names = vec![name.clone()];
                    if name.starts_with("refs/tags/") {
                        names.push(format!("{name}^{{}}"));
                    }
                    names
                } else {
                    vec![
                        format!("refs/heads/{name}"),
                        format!("refs/tags/{name}"),
                        format!("refs/tags/{name}^{{}}"),
                    ]
                }
            }
        }
    }
}
pub fn advertised_commit(reference: &Reference, bytes: &[u8]) -> Result<ObjectId> {
    if let Reference::Commit(id) = reference {
        return if bytes.is_empty() {
            Ok(id.clone())
        } else {
            Err(invalid())
        };
    }
    if bytes.len() > 64 * 1024 {
        return Err(invalid());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    if !text.ends_with('\n') || text.lines().count() > 512 {
        return Err(invalid());
    }
    let names = reference.patterns();
    let mut objects = std::collections::BTreeMap::new();
    let mut symbols = BTreeSet::new();
    for line in text.lines() {
        let (value, name) = line.split_once('\t').ok_or_else(invalid)?;
        if !names.iter().any(|n| n == name) {
            return Err(invalid());
        }
        if let Some(target) = value.strip_prefix("ref: ") {
            if !target.starts_with("refs/")
                || !matches!(Reference::parse(Some(target))?, Reference::Named(_))
                || !symbols.insert(name)
            {
                return Err(invalid());
            }
        } else if objects.insert(name, ObjectId::parse(value)?).is_some() {
            return Err(invalid());
        }
    }
    let choices: Vec<_> = objects
        .keys()
        .filter(|name| !name.ends_with("^{}"))
        .copied()
        .collect();
    let [selected] = choices.as_slice() else {
        return Err(Error::attention(
            "ambiguous_source_ref",
            "引用缺失或同时匹配分支与标签，请使用明确引用或提交",
        ));
    };
    if symbols.iter().any(|name| !objects.contains_key(name)) {
        return Err(invalid());
    }
    let peeled = format!("{selected}^{{}}");
    if objects
        .keys()
        .any(|name| name.ends_with("^{}") && **name != peeled)
    {
        return Err(invalid());
    }
    objects
        .get(peeled.as_str())
        .or_else(|| objects.get(selected))
        .cloned()
        .ok_or_else(invalid)
}
