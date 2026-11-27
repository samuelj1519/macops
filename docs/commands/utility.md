# 查询、推荐和恢复

| 命令 | 用途和省略行为 |
| --- | --- |
| `mo` | 终端打开本机清单。重定向时查看可读清单，也可显式用 `--json` |
| `mo search NAME`、`mo NAME` | 跨类型查名称或 ID |
| `mo recommend NAME` | 查看本机匹配项、可执行候选和公开安装说明。目标可以是软件名、公开 GitHub 仓库名或链接 |
| `mo recommend --setup` | 在终端配置和测试推荐模型。单独使用 |
| `mo history [QUERY]` | 查看历史。按对象名、目标 ID 或操作编号筛选 |
| `mo recover [ID]` | 省略 ID 时列出待核对项。指定 ID 时检查原执行结果并按证据收口 |
| `mo recover ID --dry-run` | 查看原计划和范围 |
| `mo recover ID --mark-failed` | 已在外部确认原结果且决定保留现状时，将支持的操作记为失败。按提示确认 |

## 推荐参数

| 参数 | 用途 |
| --- | --- |
| `--github OWNER/REPO` | 为短名称指定公开证据仓库 |
| `--evidence FILE` | 读本机来源证据，与 `--github` 二选一。JSON 需有 `schema_version: 1` 和 `sources` 数组，每项含 `subject`、HTTPS `url` 和 `excerpt` |
| `--model` | 明确请求模型解释随程序提供的公开证据。远端模型需要可用的凭据。预演跳过模型请求 |
| `--keychain-account NAME` | 搭配 `--model` 选择旧 DeepSeek 钥匙串账户 |
| `--setup` | 打开设置向导。与目标、`--evidence`、`--github`、`--model` 互斥 |

有可执行候选时，终端可继续确认写入位置。手动安装建议将命令和来源展示给读者。远端辅助不可用时，仓库推荐可继续使用本地依据。 [推荐流程](../flows/recommend.md)

重复适用的写命令通常会自动检查旧结果。核对原操作时使用同一 HOME、目录选项和原生管理器范围。核对结果已确定且还有原生阶段需要执行时，按提示使用 `mo agent resume ID`。 [恢复流程](../flows/recovery.md)
