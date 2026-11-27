# 技能和插件命令

先用 `mo skill --details` 核对成员名、来源、安装位置和启用状态。技能可以直接来自 GitHub 仓库。原生插件的可用安装方式取决于插件市场、客户端和配置。

| 命令 | 用途和参数 |
| --- | --- |
| `mo skill [NAME_OR_ID]` | 浏览或查询现有能力。列表可加 `--details` 与可重复的 `--only NAME` |
| `mo skill install OWNER/REPO[@REF]` | 核对来源和目标，选择成员并安装。支持公开 GitHub 链接。用 `@REF` 选标签、分支或提交 |
| `mo skill upgrade [NAME]` | 按技能名、唯一 ID、来源分组或支持的 GitHub 仓库更新。加 `@REF` 指定目标版本或引用 |
| `mo skill uninstall NAME` | 卸载明确的安装。旧别名为 `remove` |
| `mo skill enable NAME`、`mo skill disable NAME` | 启停支持维护的已安装插件 |
| `mo skill doctor [NAME]` | 查看本机证据，可重复加 `--only NAME` |
| `mo skill refresh [NAME]`、`mo skill outdated [NAME]` | 刷新来源索引，或读取现有索引的可更新项 |
| `mo skill market [HARNESS_OR_PROFILE]` | 查看市场与原生入口。Cursor 账户市场需显式加 `--online`，由原生 CLI 查询并可能更新其缓存 |
| `mo skill resources` | 返回能力资源快照 |

## 选择 Agent 和写入范围

| 参数 | 适用与默认行为 |
| --- | --- |
| `--agent NAME` | 安装、升级、卸载、启用和停用时选一个 Agent。可重复。值用 `mo agent` 中的名称 |
| `--agent all` | 单独使用，选择已发现的受支持客户端配置。结果仍可标出未安装或不支持该操作的目标 |
| `--shared` | 明确同意写入共享目录。共享技能的变更可能被名单外 Agent 读取，脚本选择 Agent 后还应在需要时显式确认共享范围 |
| `--only NAME`、`--all` | 在支持来源维护的命令中选技能成员。可重复传 `--only`。这里的 `--all` 选择成员，`--agent all` 选择 Agent |
| `--revision SHA` | 在支持固定来源的命令中指定完整提交 SHA。升级仓库时也可用 `OWNER/REPO@REF` |
| `--switch-source` | 明确允许受支持的 Claude 插件更换来源。必须能核对旧市场影响范围及与上游相同的市场名、插件名。详见 [换源流程](../flows/capabilities.md#claude-code-换源并升级) |

终端中，按 Agent 编排的入口在省略目标时提供多选。脚本应给出目标和适用的共享授权。已有无 Agent 参数的共享技能脚本沿用原来的作用域。

升级时，`--agent`、`--shared`、`--switch-source` 属于按 Agent 维护。使用这些选项时，成员目标放在位置参数中。`--only`、`--all` 和旧策略的 `--change-id` 用于原有来源维护，不能与这组按 Agent 选项混用。

```sh
mo skill install owner/repo --agent claude-mi --agent claude-glm --dry-run
mo skill upgrade owner/repo --agent all --dry-run
```

用 macops 安装且核验成功的技能会记录后续维护证据。已有的受支持原生安装在维护时根据实际状态查找。首次维护兼容的共享技能可能先确认现有安装基线。遇到不受支持的原生安装方式，按 `mo recommend OWNER/REPO` 给出的来源指引处理。

插件可按名称查找。选中的配置安装了来自不同市场的同名插件时，使用清单显示的完整名称，例如 `widget@fixture-market`，再预演操作。

## 配方与兼容入口

| 命令 | 用途 |
| --- | --- |
| `mo skill install --recipe /absolute/recipe.json` | 为已有自动化执行本机配方，与 GitHub 位置参数二选一 |
| `mo skill adopt NAME [--recipe FILE]` | 旧接管脚本入口，显式配方路径须为绝对路径 |
| `mo skill plugins list [--instance INSTANCE]` · `check RECIPE` · `upgrade RECIPE` | 核对实例、固定配方或预演并执行受支持的同源重装。见 [原生插件配方](native-plugin-recipes.md) |
| `mo skill sync --all --dry-run` · `mo skill sync --only profile:mi --dry-run` | 同步技能链接或已声明配置字段。可重复的 `--only` 用来组合目标 |
| `mo skill policy check` · `sync [--change-id ID]` · `recover [--change-id ID]` | 检查、同步或核对旧策略记录。`--change-id` 来自原操作 |

[能力维护流程](../flows/capabilities.md)
