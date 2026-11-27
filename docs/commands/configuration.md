# 配置命令

chezmoi 管理源保存声明。本机文件是实际生效的目标。先用 `mo config enroll` 记录范围，再用 `mo config list --refresh` 建立可查清单。

| 命令 | 用途和省略行为 |
| --- | --- |
| `mo config [NAME_OR_ID]` | 浏览或查看清单中的配置详情 |
| `mo config enroll` | 登记适用的 chezmoi 管理范围 |
| `mo config list [--refresh]` | 查看已登记的清单。加 `--refresh` 时调用原生工具并更新清单 |
| `mo config where QUERY` | 按文件名或路径片段寻找配置 |
| `mo config source-path [TARGET]` | 查管理源目录，或查看一个目标对应的管理源路径 |
| `mo config status [TARGET...]` | 比较指定路径。省略路径时检查登记范围 |
| `mo config diff [TARGET]` | 查看应用后差异，省略时查看整个登记范围 |
| `mo config verify TARGET...` | 核验一个或多个路径与管理源一致 |
| `mo config apply TARGET` | 将管理源应用到指定本机路径或清单中的唯一 ID |
| `mo config adopt /absolute/path` | 把已有普通文件纳入支持的静态管理源。预演后确认会增加的明文文件 |
| `mo config forget ID` | 停止管理一个源条目，保留实际配置文件 |
| `mo config restore ID` | 对支持的旧配置事务按历史操作编号还原。原生应用的修复走前向配置操作 |
| `mo config reconcile [TOOL]` | 不带工具时列出 mise 本机固定版本与管理源差异。指定工具时请求对账 |
| `mo config resources` | 返回配置资源快照 |

```sh
mo config source-path /absolute/target
mo config diff /absolute/target
mo config apply /absolute/target --dry-run
```

## 配置范围

同时提供 `--chezmoi-source SOURCE`、`--chezmoi-destination DESTINATION` 和 `--chezmoi-config CONFIG`，可为支持的命令明确选择原生上下文。它们分别指向管理源、实际目标根和原生配置。先用查询命令确认选中的范围。

原生操作沿用该配置的模板、过滤规则、hooks 及所需的网络和缓存行为。部分证据仍可能随结果一起返回，退出码 3 表示需要处理。 [配置应用、登记与恢复](../flows/configuration.md)
