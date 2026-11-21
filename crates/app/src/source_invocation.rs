use crate::{
    capability_store as files,
    error::{Error, Result},
    policy::{
        capability_tx::Namespace,
        invocation::{self, MetadataKind},
        source_invocation::{self as policy, Applied},
        source_tree::{Content, Position, Slot, Tree},
        source_tx::UpgradedRoot,
    },
};
use std::path::Path;
fn invalid() -> Error {
    policy::drift()
}
pub fn metadata(slot: &Slot, tree: &Tree, relative: &str) -> Result<Option<Vec<u8>>> {
    if crate::source_tree::observe(slot)?.as_ref() != Some(tree) {
        return Err(invalid());
    }
    let Some(entry) = tree.entries.get(relative) else {
        return Ok(None);
    };
    let Content::File { sha256, size } = &entry.content else {
        return Err(invalid());
    };
    let namespace = Namespace {
        ancestor: slot.project(tree),
        remaining: relative.split('/').map(str::to_owned).collect(),
    };
    let (state, raw) = files::observe(&namespace, 1024 * 1024)?;
    if !matches!(state,crate::policy::capability_tx::State::File{identity,sha256:ref actual,mode} if identity==entry.identity && actual==sha256 && mode==entry.mode)
        || raw.as_ref().is_none_or(|raw| raw.len() as u64 != *size)
    {
        return Err(invalid());
    }
    if crate::source_tree::observe(slot)?.as_ref() != Some(tree) {
        return Err(invalid());
    }
    Ok(raw)
}
fn old_upstream(policy: &Applied, logical: &Slot, slot: &Slot, tree: &Tree) -> Result<bool> {
    if let Some(row) = policy.book.files.get(&logical.path().join("SKILL.md")) {
        let original = row.bytes()?.0.ok_or_else(invalid)?;
        invocation::manual_frontmatter(&original)
    } else {
        invocation::manual_frontmatter(
            metadata(slot, tree, "SKILL.md")?
                .as_deref()
                .ok_or_else(invalid)?,
        )
    }
}
fn current_at(policy: &Applied, logical: &Slot, slot: &Slot, tree: &Tree) -> Result<()> {
    let declaration = policy.declaration.as_ref();
    let needs_upstream = declaration.is_some_and(|decl| {
        decl.mirror_standalone_upstream_manual
            && !logical.name.to_ascii_lowercase().starts_with("devnorm")
            && !decl.standalone_manual_only.contains(&logical.name)
    });
    let upstream = needs_upstream
        .then(|| old_upstream(policy, logical, slot, tree))
        .transpose()?;
    let wanted = policy::intent(declaration, &logical.name, upstream)?.manual();
    for (path, row) in &policy.book.files {
        let Ok(relative) = path.strip_prefix(logical.path()) else {
            continue;
        };
        row.bytes()?;
        let relative = relative.to_str().ok_or_else(invalid)?;
        let entry = tree.entries.get(relative).ok_or_else(invalid)?;
        if entry.mode != row.applied_mode
            || !matches!(&entry.content,Content::File{sha256,..} if sha256==&row.applied_hash)
        {
            return Err(Error::attention(
                "invocation_overlay_drift",
                "来源升级要求活动 overlay 精确匹配旧 applied 字节和权限",
            ));
        }
        if !wanted {
            return Err(policy::sync_required());
        }
    }
    if wanted {
        for (relative, kind) in [
            ("SKILL.md", MetadataKind::Frontmatter),
            ("agents/openai.yaml", MetadataKind::CodexPolicy),
        ] {
            if policy
                .book
                .files
                .contains_key(&logical.path().join(relative))
            {
                continue;
            }
            let before = metadata(slot, tree, relative)?;
            if !matches!(invocation::transform(kind,before.as_deref()),Ok(ref applied) if Some(applied.as_slice())==before.as_deref())
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
fn known_slot(root: &UpgradedRoot, after: bool) -> Result<&Slot> {
    match crate::source_tree::position(&root.change)? {
        Position::Before => Ok(if after {
            &root.change.retained
        } else {
            &root.change.live
        }),
        Position::OwnAfter => Ok(if after {
            &root.change.live
        } else {
            &root.change.retained
        }),
        Position::Unknown => Err(invalid()),
    }
}
pub fn verify(
    home: &Path,
    policy: &Applied,
    roots: &[UpgradedRoot],
    after_overlay: Option<&[u8]>,
) -> Result<()> {
    let after = after_overlay
        .map(|raw| invocation::record(home, raw))
        .transpose()?;
    for root in roots {
        let before_slot = known_slot(root, false)?;
        let logical = Slot {
            parent: root.change.live.parent.clone(),
            name: root.change.live.name.clone(),
        };
        current_at(policy, &logical, before_slot, &root.change.before)?;
        let declaration = policy.declaration.as_ref();
        let needs_upstream = declaration.is_some_and(|decl| {
            decl.mirror_standalone_upstream_manual
                && !logical.name.to_ascii_lowercase().starts_with("devnorm")
                && !decl.standalone_manual_only.contains(&logical.name)
        });
        let upstream = if needs_upstream {
            let raw = if let Some(row) = after
                .as_ref()
                .and_then(|book| book.files.get(&logical.path().join("SKILL.md")))
            {
                row.bytes()?.0
            } else {
                metadata(known_slot(root, true)?, &root.change.after, "SKILL.md")?
            };
            Some(invocation::manual_frontmatter(
                raw.as_deref().ok_or_else(invalid)?,
            )?)
        } else {
            None
        };
        if policy::intent(declaration, &logical.name, upstream)? != root.invocation {
            return Err(invalid());
        }
    }
    Ok(())
}
