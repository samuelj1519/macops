# 高级维护

## 目录与清理

| 命令 | 用途 |
| --- | --- |
| `mo dirs --roots` | 查生效的 HOME、设置、配置、数据、状态和缓存目录 |
| `mo dirs ID` | 查指定对象使用的位置 |
| `mo cleanup macops:cache --dry-run` | 列出通过核验的自有缓存候选 |
| `mo cleanup macops:cache` | 按清理计划删除符合条件的自有缓存 |

`mo dirs --roots` 与对象 ID 二选一。对原生工具的共享缓存，按工具的维护方式处理。操作日志、回执和待核对材料见 [数据位置](../design/storage.md)。

## 导出和核对清单

```sh
mo manifest export --json | jq .data.manifest > manifest.json
mo manifest plan manifest.json
mo manifest replay manifest.json ID --dry-run
```

导出的清单带对象身份、范围、配方摘要和版本证据。`plan FILE` 比较本地差异并给出重放资格。`replay FILE ID` 一次选择一个符合条件的 npm 项，要求相同范围与正式配方、明确的发布或预发布版本，以及已收口的操作历史。使用实际重放前先查计划。 [清单重放](../flows/maintenance.md#清单重放)

## 旧数据迁移

| 命令 | 用途 |
| --- | --- |
| `mo migrate preflight` | 检查旧数据、原地保留项、归档计划与阻断条件 |
| `mo migrate run` | 执行通过预检的转换 |
| `mo migrate verify` | 检查新数据与旧源是否分叉 |
| `mo migrate recover MIGRATION_ID` | 按原迁移记录核对和收口 |

这些命令用于旧数据转换。迁移未收口时，普通命令可能提示先核对迁移。 [迁移流程](../flows/maintenance.md#旧数据迁移)
