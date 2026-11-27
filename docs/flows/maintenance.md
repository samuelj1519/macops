# 清单、迁移和缓存维护

这些命令主要处理 macops 的自有记录。先用 `mo dirs --roots` 确认范围。 [高级维护命令](../commands/maintenance.md) · [数据位置](../design/storage.md)

## 清单重放

导出包含正式声明、登记的原生能力、可核验的配置缓存和安装回执。导入时检查类型、大小、摘要和作用域。预演分别显示本机差异和重放资格。

重放选择一个符合条件的 npm 项。候选须与正式配方、授权范围和明确版本一致。实际重放前重新核对条件。目标已满足时，在锁内以实际观察确认。否则委托已有安装或升级路径。发现未决操作或证据变化时先处理原结果或刷新计划。 [清单实现](../../crates/app/src/operations/manifest.rs) · [清单模型](../models/control_plane/Replay.tla)

## 旧数据迁移

先用 `mo migrate preflight` 查看哪些旧记录需要转换、保留或归档。通过预检后，`migrate run` 使用单独的迁移账本记录工作。迁移会检查旧写入入口是否已停写，并对比原始证据。

发布前遇到中断时，按事务记录核对原位置与暂存内容，决定是否可回退。发布后核对新记录及旧源差异，再收口。使用 `mo migrate recover MIGRATION_ID` 处理原迁移，使用 `mo migrate verify` 检查分叉。普通维护遇到未完成迁移时应暂停。 [迁移入口](../../crates/app/src/migration/mod.rs) · [迁移事务](../../crates/app/src/migration/migration_tx.rs) · [迁移与旧写入模型](../models/migration/)

## 清理自有缓存

`mo cleanup macops:cache --dry-run` 列出通过核验的自有候选。实际清理在锁内检查候选仍符合条件，隔离并删除通过复验的对象。当前索引、外部拥有的内容和待核对所需证据应继续保留。共享或原生缓存按对应工具的维护方式处理。 [清理实现](../../crates/app/src/catalog/cache.rs) · [缓存模型](../models/cache/Cache.tla)
