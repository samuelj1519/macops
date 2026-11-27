# Agent 命令

Agent 的启动名称、CLI 版本和配置目录是三个不同的信息。例如 `claude-mi` 与 `claude-glm` 可共用 Claude Code 的可执行文件，各自保留插件和模型设置。

初次打开时，配置行显示实际启动程序对应的 mise 安装版本。显示「待核验」时，可按 `r` 补查。mo 把受支持命令报告的版本填入使用同一程序的配置行。`mo agent tools` 可单独查询这些命令的版本。

| 命令 | 用途与参数 |
| --- | --- |
| `mo agent [PROFILE]` | 浏览配置或按名称、唯一 ID 查详情 |
| `mo agent tools` | 查支持的 CLI 及本机、管理源固定版本 |
| `mo agent add NAME --config /absolute/path` | 登记发现不到的已有配置。输入实际启动命令名和目录。无法推断客户端时加 `--type claude`、`codex` 或 `cursor` |
| `mo agent install TOOL [VERSION]` | 安装支持的 Agent CLI。用对应命令的帮助和预演核对版本选择 |
| `mo agent upgrade [TOOL]` | 升级指定工具。省略工具时检查支持工具的配置差异 |
| `mo agent resume ID` | 确认原安装剩余阶段。先加 `--dry-run` 核对原来源、目标与启停设置 |
| `mo agent launch PROFILE` | 启动由 macops 管理的单独配置。Codex 可加 `--native-profile NAME` 选择配置内已有的原生 profile |
| `mo agent uninstall PROFILE` | 卸载 macops 创建并管理的单独配置，移入归档。加 `--dry-run` 核对位置 |
| `mo agent resources` | 返回 Agent 配置资源快照 |

内置 CLI 名包括 `mimo`、`codex`、`claude`、`cursor`、`kimi`、`dsh` 和 `codegraph`。Cursor 的工具参数是 `cursor`，可执行命令是 `cursor-agent`。`dsh`、`codegraph` 安装需要明确版本。适用的 mise 安装或升级可加 `--local`，只更新本机安装和本机 mise 设置。它保留管理源中的声明。旧参数 `--force-live` 是兼容别名。 [Agent CLI 升级](../flows/agents.md#安装与升级-cli)

```sh
mo agent upgrade codex --dry-run
mo agent launch PROFILE --dry-run --json
```

已有的正常配置用平时使用的命令启动。在 macops 管理的单独配置详情中，可按 `e` 设置以后由 macops 启动时的插件偏好，按 `l` 从当前项目启动。脚本使用 JSON 查看启动计划时须加 `--dry-run`。

## 凭据元信息

| 命令 | 用途 |
| --- | --- |
| `mo agent credential list` | 查看可用凭据状态 |
| `mo agent credential check [PROFILE]` | 核对指定或全部支持档案的状态 |
| `mo agent credential add PROFILE ALIAS` | 查看新增凭据所需的原生工具指引 |
| `mo agent credential activate PROFILE ALIAS` | 查看激活指定凭据的指引 |

凭据档案值为 `glm`、`kimi`、`mi`、`zen`。别名用字母、数字、点、下划线和短横线。命令只报告元信息，输入实际凭据时使用提示中的原生工具。推荐模型使用独立的 [设置入口](../flows/recommend.md#推荐模型设置)。
