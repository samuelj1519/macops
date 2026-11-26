use super::{Binding, Cache, Managed, io, query_differences, query_managed, validate_cache};
use crate::{
    browser::{Item, View},
    context::Context,
    domain::FileId,
    error::{Error, Result},
    operations::store::Store,
    platform::fs::{Bindings, BoundTarget, Directory, EntryKind, ObservedRegular},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub(crate) struct ConfigBrowser {
    cache: Option<Cache>,
    owner: Option<Owner>,
    knowledge: Knowledge,
    refresh_blocked: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Knowledge {
    Cached,
    Fresh,
    MappingOnly,
    Historical,
}

#[derive(Clone)]
struct Owner {
    record: BoundTarget,
    identity: FileId,
    binding: Binding,
}

impl Owner {
    fn load(ctx: &Context) -> Result<Option<Self>> {
        if !ctx.state.exists() {
            return Ok(None);
        }
        let dir = Directory::open(&ctx.state)?;
        if dir.path != ctx.state {
            return Err(io::changed());
        }
        let record = Bindings::new(2).relative(&dir, Path::new("chezmoi-owner.json"))?;
        let (bytes, metadata) = match record.observe_regular(16 * 1024 * 1024)? {
            ObservedRegular::Missing => return Ok(None),
            ObservedRegular::Regular { bytes, metadata } if metadata.mode == 0o600 => {
                (bytes, metadata)
            }
            _ => return Err(io::changed()),
        };
        let binding: Binding = serde_json::from_value(crate::policy::json::parse(&bytes)?)
            .map_err(|_| io::changed())?;
        super::policy::validate(&binding)?;
        if binding.scope != ctx.scope || binding.record_state != ctx.state {
            return Err(io::changed());
        }
        for (selected, recorded) in [
            (&ctx.chezmoi.source, &binding.configured_source),
            (&ctx.chezmoi.destination, &binding.destination),
            (&ctx.chezmoi.config, &binding.config),
        ] {
            if selected
                .as_ref()
                .is_some_and(|selected| selected != recorded)
            {
                return Err(io::changed());
            }
        }
        Ok(Some(Self {
            record,
            identity: metadata.identity,
            binding,
        }))
    }

    fn verify(&self) -> Result<()> {
        if self
            .record
            .metadata()?
            .is_none_or(|metadata| metadata.mode != 0o600)
        {
            return Err(io::changed());
        }
        let bytes = self
            .record
            .read_selected(&self.identity, 16 * 1024 * 1024)
            .map_err(|_| io::changed())?;
        let current: Binding = serde_json::from_value(crate::policy::json::parse(&bytes)?)
            .map_err(|_| io::changed())?;
        if current != self.binding {
            return Err(io::changed());
        }
        io::verify(&self.binding)
    }
}

fn same_authority(old: &Binding, current: &Binding) -> bool {
    old.scope == current.scope
        && old.lookup == current.lookup
        && old.program == current.program
        && old.version == current.version
        && old.anchors == current.anchors
        && old.configured_source == current.configured_source
        && old.effective_source == current.effective_source
        && old.destination == current.destination
        && old.config == current.config
        && old.cache == current.cache
        && old.persistent_state == current.persistent_state
        && old.record_state == current.record_state
}

pub(crate) struct ViewerFile {
    owner: Owner,
    binding: Binding,
    target: BoundTarget,
    identity: FileId,
    path: PathBuf,
}

fn anchored(binding: &Binding, path: &Path) -> Result<Directory> {
    let root = Directory::open(path)?;
    if root.path != path
        || !binding
            .anchors
            .iter()
            .any(|stamp| stamp.path == path && stamp.identity.as_ref() == Some(root.identity()))
    {
        return Err(io::changed());
    }
    Ok(root)
}

impl ViewerFile {
    pub(crate) fn read(&self) -> Result<String> {
        self.owner.verify()?;
        io::verify(&self.binding)?;
        let bytes = self.target.read_selected(&self.identity, 256 * 1024)?;
        io::verify(&self.binding)?;
        self.owner.verify()?;
        let content = String::from_utf8(bytes)
            .map_err(|_| Error::attention("file_not_text", "所选文件不是 UTF-8 文本"))?;
        if content.contains('\0') {
            return Err(Error::attention("file_not_text", "所选文件包含二进制数据"));
        }
        Ok(content)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl ConfigBrowser {
    pub(crate) fn load(ctx: &Context) -> Result<Self> {
        let cache: Option<Cache> = io::read(ctx, "chezmoi-metadata.json")?;
        let owner = Owner::load(ctx)?;
        let knowledge = if let Some(cache) = &cache {
            validate_cache(ctx, cache)?;
            if owner
                .as_ref()
                .is_some_and(|owner| same_authority(&cache.binding, &owner.binding))
            {
                Knowledge::Cached
            } else {
                Knowledge::Historical
            }
        } else {
            Knowledge::Cached
        };
        Ok(Self {
            cache,
            owner,
            knowledge,
            refresh_blocked: false,
        })
    }

    pub(crate) fn view(&self) -> View {
        let Some(cache) = &self.cache else {
            return View {
                scope: "chezmoi".into(),
                items: vec![],
                coverage: if self.owner.is_some() {
                    "pending"
                } else {
                    "unregistered"
                },
                difference_coverage: "unknown",
                observed_at: None,
                note: if self.owner.is_some() {
                    "尚未确认受管配置清单，按 r 刷新"
                } else {
                    "先用 mo config enroll 登记 chezmoi"
                }
                .into(),
                incomplete: true,
            };
        };
        let pending =
            self.knowledge == Knowledge::Cached && cache.entries.is_empty() && !cache.complete;
        let differences =
            !pending && matches!(self.knowledge, Knowledge::Cached | Knowledge::Fresh);
        let source = anchored(&cache.binding, &cache.binding.effective_source).ok();
        let destination = anchored(&cache.binding, &cache.binding.destination).ok();
        let historical =
            self.knowledge == Knowledge::Historical || source.is_none() || destination.is_none();
        let coverage = if historical {
            "historical"
        } else if pending {
            "pending"
        } else if self.knowledge == Knowledge::Cached {
            "cached"
        } else {
            "observed"
        };
        let difference_coverage = if historical || !differences {
            "unknown"
        } else if cache.binding.filtered {
            "partial"
        } else {
            coverage
        };
        let items = visible_configs(
            cache,
            source.as_ref(),
            destination.as_ref(),
            historical,
            differences,
        );
        let note = if historical {
            "绑定已变化，仅显示历史清单"
        } else if pending {
            "尚未确认受管配置清单，按 r 刷新"
        } else if !differences {
            "已确认受管路径，差异状态待查"
        } else if cache.binding.filtered {
            "受管配置清单，差异核验覆盖部分类型"
        } else if items.iter().all(|item| item.directory) {
            "当前范围无受管文件，按 d 查看目录"
        } else {
            "受管配置清单"
        };
        View {
            scope: cache.binding.destination.display().to_string(),
            items,
            coverage,
            difference_coverage,
            observed_at: (!pending).then_some(cache.observed_at),
            note: note.into(),
            incomplete: historical || pending || !differences || cache.binding.filtered,
        }
    }

    pub(crate) fn refresh(&mut self, ctx: &Context) -> Result<()> {
        if self.refresh_blocked {
            return Err(Error::attention(
                "refresh_uncertain",
                "本会话无法再次刷新，先核对原查询",
            ));
        }
        let owner = self.owner.as_ref().ok_or_else(|| {
            Error::attention("chezmoi_binding_unresolved", "请先运行 mo config enroll")
        })?;
        owner
            .verify()
            .inspect_err(|_| self.knowledge = Knowledge::Historical)?;
        let store = Store::lock(ctx)?;
        store.check_pending()?;
        owner
            .verify()
            .inspect_err(|_| self.knowledge = Knowledge::Historical)?;
        self.refresh_blocked = true;
        let binding = owner.binding.clone();
        let env = io::environment(ctx);
        let entries = query_managed(ctx, &binding, &env).inspect_err(|_| {
            if owner.verify().is_err() || store.verify_namespace().is_err() {
                self.knowledge = Knowledge::Historical;
            }
        })?;
        let (differences, finished) = query_differences(ctx, &binding, &env, &entries);
        owner
            .verify()
            .and_then(|()| store.verify_namespace())
            .inspect_err(|_| self.knowledge = Knowledge::Historical)?;
        let status_known = differences.is_ok();
        let cache = Cache {
            source_publication: None,
            complete: status_known && !binding.filtered,
            binding,
            entries,
            observed_at: crate::operations::store::now(),
            status: differences.unwrap_or_default(),
        };
        if status_known {
            Store::atomic(&store.root, "chezmoi-metadata.json", &cache)?;
        }
        self.cache = Some(cache);
        self.knowledge = if status_known {
            Knowledge::Fresh
        } else {
            Knowledge::MappingOnly
        };
        self.refresh_blocked = !finished;
        Ok(())
    }

    pub(crate) fn select_file(&self, id: &str, source: bool) -> Result<ViewerFile> {
        let unavailable =
            || Error::attention("file_unavailable", "需要当前作用域内已核验的普通文件");
        if self.knowledge == Knowledge::Historical {
            return Err(unavailable());
        }
        let owner = self.owner.as_ref().ok_or_else(unavailable)?;
        owner.verify()?;
        let cache = self.cache.as_ref().ok_or_else(unavailable)?;
        let managed = cache.entries.get(id).ok_or_else(unavailable)?;
        let binding = &cache.binding;
        io::verify(binding)?;
        let (base, path) = if source {
            (&binding.effective_source, &managed.source_absolute)
        } else {
            (&binding.destination, &managed.absolute)
        };
        let relative = path.strip_prefix(base).map_err(|_| io::changed())?;
        let target = Bindings::new(64).relative(&anchored(binding, base)?, relative)?;
        let metadata = target.metadata()?.ok_or_else(unavailable)?;
        if metadata.kind != EntryKind::File {
            return Err(unavailable());
        }
        io::verify(binding)?;
        owner.verify()?;
        Ok(ViewerFile {
            owner: owner.clone(),
            binding: binding.clone(),
            target,
            identity: metadata.identity,
            path: path.clone(),
        })
    }
}

fn visible_configs(
    cache: &Cache,
    source: Option<&Directory>,
    destination: Option<&Directory>,
    historical: bool,
    differences: bool,
) -> Vec<Item> {
    let mut bindings = Bindings::new(64);
    let status: BTreeSet<_> = cache.status.iter().map(|row| &row.path).collect();
    let mut items = Vec::new();
    for (id, entry) in &cache.entries {
        let actual = destination.and_then(|root| {
            bindings
                .relative(root, Path::new(id))
                .ok()?
                .metadata()
                .ok()?
        });
        let source_metadata = source.and_then(|root| {
            bindings
                .relative(root, &entry.source_relative)
                .ok()?
                .metadata()
                .ok()?
        });
        let directory = [actual.as_ref(), source_metadata.as_ref()]
            .iter()
            .any(|m| m.is_some_and(|m| m.kind == EntryKind::Directory))
            && [actual.as_ref(), source_metadata.as_ref()]
                .iter()
                .flatten()
                .all(|m| m.kind == EntryKind::Directory);
        let summary = if historical {
            "历史记录"
        } else if differences && status.contains(&entry.absolute) {
            "上次查询有差异"
        } else if actual.is_some() {
            "已存在"
        } else {
            "状态待查"
        };
        items.push(config_item(
            id,
            entry,
            summary,
            actual.as_ref().map_or("unknown", |m| m.kind.as_str()),
            directory,
        ));
    }
    items
}

fn config_item(id: &str, entry: &Managed, summary: &str, kind: &str, directory: bool) -> Item {
    Item {
        id: id.into(),
        name: id.into(),
        summary: summary.into(),
        managed: true,
        management: crate::domain::resource::Management::Native,
        directory,
        path: Some(entry.absolute.clone()),
        source: Some(entry.source_absolute.clone()),
        agents: vec![],
        loaded: None,
        inventory: None,
        details: vec![
            "管理器 chezmoi".into(),
            format!("目标 {}", entry.absolute.display()),
            format!("管理源 {}", entry.source_absolute.display()),
            format!("类型 {kind} · {summary}"),
        ],
    }
}
