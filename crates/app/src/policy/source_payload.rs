use crate::{
    error::{Error, Result},
    policy::{
        source_git::{RelativePath, Repository},
        source_tree::{Content, Shape, ShapeEntry},
    },
};
use serde_json::Value;
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Directory { mode: u32 },
    File { mode: u32, bytes: Vec<u8> },
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Payload {
    pub entries: BTreeMap<String, Entry>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub path: String,
    pub old: String,
    pub new: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rules {
    pub repository: Repository,
    pub path: RelativePath,
    pub licenses: Vec<RelativePath>,
    pub exclude: Vec<String>,
    pub edits: Vec<Edit>,
    pub manual: bool,
}
fn invalid() -> Error {
    Error::attention(
        "invalid_source_payload",
        "来源载荷、声明变换或文件预算无法验证",
    )
}
fn path_list(value: Option<&Value>, default: &[&str]) -> Result<Vec<RelativePath>> {
    let values = if let Some(value) = value {
        value
            .as_array()
            .ok_or_else(invalid)?
            .iter()
            .map(|v| v.as_str().map(str::to_owned).ok_or_else(invalid))
            .collect::<Result<Vec<_>>>()?
    } else {
        default.iter().map(|s| (*s).into()).collect()
    };
    if values.len() > 128 {
        return Err(invalid());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut paths = vec![];
    for value in &values {
        let path = RelativePath::parse(value)?;
        if value == "." || !seen.insert(value) {
            return Err(invalid());
        }
        paths.push(path);
    }
    Ok(paths)
}
impl Rules {
    pub fn native(repository: Repository, path: RelativePath) -> Self {
        Self {
            repository,
            path,
            licenses: vec![],
            exclude: vec![],
            edits: vec![],
            manual: false,
        }
    }
    pub fn declared(value: &Value) -> Result<Self> {
        let row = value.as_object().ok_or_else(invalid)?;
        let allowed = [
            "kind",
            "repository",
            "path",
            "branch",
            "revision",
            "license_files",
            "exclude",
            "edits",
            "manual_only",
        ];
        if row.keys().any(|key| !allowed.contains(&key.as_str()))
            || row.get("kind").and_then(Value::as_str) != Some("github-skill")
        {
            return Err(invalid());
        }
        let repository = Repository::parse(
            row.get("repository")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?,
        )?;
        let path = RelativePath::parse(
            row.get("path")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?,
        )?;
        let licenses = path_list(row.get("license_files"), &["LICENSE"])?;
        let exclude = path_list(row.get("exclude"), &[])?
            .iter()
            .map(|path| {
                path.parts()
                    .iter()
                    .map(crate::policy::source_git::Component::as_str)
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .collect();
        let manual = row
            .get("manual_only")
            .map(|v| v.as_bool().ok_or_else(invalid))
            .transpose()?
            .unwrap_or(false);
        let mut edits = vec![];
        if let Some(value) = row.get("edits") {
            let rows = value.as_array().ok_or_else(invalid)?;
            if rows.len() > 128 {
                return Err(invalid());
            }
            for value in rows {
                let edit = value.as_object().ok_or_else(invalid)?;
                if edit.len() != 3 {
                    return Err(invalid());
                }
                let text = |key: &str| {
                    edit.get(key)
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(invalid)
                };
                let edit = Edit {
                    path: text("path")?,
                    old: text("old")?,
                    new: text("new")?,
                };
                RelativePath::parse(&edit.path)?;
                if edit.path == "."
                    || edit.old.is_empty()
                    || edit.old.len() > 1024 * 1024
                    || edit.new.len() > 1024 * 1024
                {
                    return Err(invalid());
                }
                edits.push(edit);
            }
        }
        Ok(Self {
            repository,
            path,
            licenses,
            exclude,
            edits,
            manual,
        })
    }
    pub fn locator(
        &self,
        member: &str,
        reference: &crate::policy::source_git::Reference,
    ) -> Result<String> {
        let path = self
            .path
            .parts()
            .iter()
            .map(super::source_git::Component::as_str)
            .collect::<Vec<_>>();
        let licenses = self
            .licenses
            .iter()
            .map(|p| {
                p.parts()
                    .iter()
                    .map(super::source_git::Component::as_str)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let edits = self
            .edits
            .iter()
            .map(|e| (&e.path, &e.old, &e.new))
            .collect::<Vec<_>>();
        crate::domain::fingerprint(&(
            "macops-source-locator-v1",
            member,
            self.repository.key(),
            path,
            licenses,
            &self.exclude,
            edits,
            self.manual,
            reference.patterns(),
        ))
    }
    pub fn excludes(&self, path: &str) -> bool {
        self.exclude
            .iter()
            .any(|p| path == p || path.strip_prefix(p).is_some_and(|s| s.starts_with('/')))
    }
}
impl Payload {
    pub fn shape(&self, owner: u32, mode: u32) -> Shape {
        Shape {
            uid: owner,
            mode,
            entries: self
                .entries
                .iter()
                .map(|(path, entry)| {
                    let (mode, content) = match entry {
                        Entry::Directory { mode } => (*mode, Content::Directory),
                        Entry::File { mode, bytes } => (
                            *mode,
                            Content::File {
                                sha256: crate::policy::invocation::hash(bytes),
                                size: bytes.len() as u64,
                            },
                        ),
                    };
                    (
                        path.clone(),
                        ShapeEntry {
                            uid: owner,
                            mode,
                            content,
                        },
                    )
                })
                .collect(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        self.shape(0, 0o700).validate(0)?;
        if !matches!(self.entries.get("SKILL.md"),Some(Entry::File{bytes,..}) if !bytes.is_empty())
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn add_license(&mut self, path: &str, entry: Entry) -> Result<()> {
        let relative = RelativePath::parse(path)?;
        let name = relative.parts().last().ok_or_else(invalid)?.as_str();
        if !matches!(entry, Entry::File { .. }) {
            return Err(invalid());
        }
        match self.entries.get(name) {
            Some(before) if before != &entry => {
                return Err(Error::attention(
                    "source_license_conflict",
                    "声明许可证与已有目标内容或权限冲突",
                ));
            }
            Some(_) => (),
            None => {
                self.entries.insert(name.into(), entry);
            }
        }
        Ok(())
    }
    pub fn transform(&mut self, rules: &Rules, licenses: &[(String, Entry)]) -> Result<()> {
        if licenses.len() != rules.licenses.len() {
            return Err(invalid());
        }
        self.entries.retain(|path, _| !rules.excludes(path));
        for ((path, entry), expected) in licenses.iter().zip(&rules.licenses) {
            if RelativePath::parse(path)? != *expected {
                return Err(invalid());
            }
            self.add_license(path, entry.clone())?;
        }
        self.validate()?;
        if rules.manual {
            self.manual()?;
        }
        for edit in &rules.edits {
            let Some(Entry::File { bytes, .. }) = self.entries.get_mut(&edit.path) else {
                return Err(invalid());
            };
            let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
            if edit.old.is_empty() || text.matches(&edit.old).count() != 1 {
                return Err(Error::attention(
                    "source_edit_mismatch",
                    "声明文本必须恰好匹配一次，未生成交付目标",
                ));
            }
            *bytes = text.replacen(&edit.old, &edit.new, 1).into_bytes();
        }
        self.validate()
    }
    fn manual(&mut self) -> Result<()> {
        use crate::policy::invocation::{MetadataKind, transform};
        let Some(Entry::File { bytes, .. }) = self.entries.get_mut("SKILL.md") else {
            return Err(invalid());
        };
        *bytes = transform(MetadataKind::Frontmatter, Some(bytes))?;
        let path = "agents/openai.yaml";
        if !self.entries.contains_key("agents") {
            self.entries
                .insert("agents".into(), Entry::Directory { mode: 0o700 });
        }
        let before = match self.entries.get(path) {
            Some(Entry::File { mode, bytes }) => Some((*mode, bytes.as_slice())),
            None => None,
            Some(_) => return Err(invalid()),
        };
        let mode = before.map_or(0o600, |b| b.0);
        let bytes = transform(MetadataKind::CodexPolicy, before.map(|b| b.1))?;
        self.entries
            .insert(path.into(), Entry::File { mode, bytes });
        Ok(())
    }
}
#[cfg(test)]
#[path = "../../tests/unit/source_payload_policy.rs"]
mod tests;
