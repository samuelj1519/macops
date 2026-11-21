use crate::{
    context::Context,
    domain::Package,
    error::{Error, Result},
    index_store::{BOOK_FILE, Book, Namespace, Registry},
    process,
    store::{self, Store},
};
use serde_json::{Value, json};

pub use crate::domain::Index;

pub fn latest(ctx: &Context, package: &Package) -> Result<String> {
    let spec = crate::providers::resolve(crate::policy::commands::available_query(
        &ctx.home, package,
    )?)?;
    let out = process::query(&ctx.scope, &spec)?;
    crate::policy::parsers::available(package, &out)
}

pub fn load(ctx: &Context) -> Result<(Index, Option<Book>)> {
    let registry = Registry::load(ctx)?;
    let Some(namespace) = Namespace::open(ctx, false, &registry)? else {
        return Ok((Index::empty(), None));
    };
    namespace.assert_current(ctx, &registry)?;
    crate::policy::cache::registered_scope(&registry, &namespace.key, &ctx.cache)?;
    let book = registry.book(&namespace.path, &namespace.identity, &namespace.key);
    let index = if let Some(ref hash) = book.current {
        namespace.read_blob::<Index>(hash)?
    } else {
        Index::empty()
    };
    index.validate()?;
    Ok((index, Some(book)))
}

pub fn read(ctx: &Context) -> Result<Index> {
    Ok(load(ctx)?.0)
}

pub fn update(ctx: &Context, packages: &[&Package], dry_run: bool) -> Result<Value> {
    for package in packages {
        crate::policy::uv_local::action(&package.provider, crate::domain::Action::Update)?;
    }
    if dry_run {
        return Ok(
            json!({"mode":"dry-run","action":"update","ids":packages.iter().map(|p|&p.id).collect::<Vec<_>>(),"network":"not-run","persistentWrites":false}),
        );
    }
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    update_locked(ctx, &store, packages, |package| latest(ctx, package))
}

pub fn update_queried(
    ctx: &Context,
    store: &Store,
    package: &Package,
    version: &str,
) -> Result<Value> {
    store.verify_namespace()?;
    update_locked(ctx, store, &[package], |_| Ok(version.into()))
}

fn update_locked(
    ctx: &Context,
    store: &Store,
    packages: &[&Package],
    mut latest: impl FnMut(&Package) -> Result<String>,
) -> Result<Value> {
    let mut registry = Registry::load(ctx)?;
    let namespace = Namespace::open(ctx, true, &registry)?
        .ok_or_else(|| Error::new("cache_namespace_missing", "没有缓存命名空间"))?;
    let mut book = registry.book(&namespace.path, &namespace.identity, &namespace.key);
    let mut index = if let Some(ref hash) = book.current {
        namespace.read_blob::<Index>(hash)?
    } else {
        Index::empty()
    };
    index.validate()?;
    let mut results = vec![];
    for package in packages {
        match latest(package) {
            Ok(version) => {
                crate::policy::cache::apply_refresh(
                    &mut index,
                    &mut book,
                    package,
                    &ctx.scope,
                    version.clone(),
                    store::now(),
                )?;
                results.push(json!({"id":package.id,"status":"updated","version":version}));
            }
            Err(error) => results.push(json!({"id":package.id,"status":"unknown","error":error})),
        }
    }
    namespace.assert_current(ctx, &registry)?;
    let (hash, created) = namespace.publish(&index)?;
    namespace.assert_current(ctx, &registry)?;
    let owned = crate::policy::cache::register_current(&mut book, &hash, created);
    registry.scopes.insert(namespace.key.clone(), book);
    Store::atomic(&store.root, BOOK_FILE, &registry)?;
    Ok(json!({"results":results,"cache":ctx.cache,"index":hash,"created":created,"owned":owned}))
}

pub fn outdated(ctx: &Context, packages: &[&Package]) -> Result<Value> {
    let (index, book) = load(ctx)?;
    let mut rows = vec![];
    for p in packages {
        let installed = crate::inventory::observe(ctx, p);
        let row = crate::policy::cache::outdated_row(
            p,
            &ctx.scope,
            &installed,
            index.entries.get(&p.id),
            book.as_ref().and_then(|b| b.queried_at.get(&p.id)).copied(),
        )?;
        rows.push(row);
    }
    Ok(json!(rows))
}
