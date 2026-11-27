# 软件命令

先用 `mo app NAME` 看安装渠道、范围和版本。有多个同名实例时，使用详情提供的唯一 ID。安装、升级、卸载和接管前可加 `--dry-run`。

| 命令 | 用途与参数 |
| --- | --- |
| `mo app [NAME_OR_ID]` | 浏览或查看一个软件实例 |
| `mo app install NAME_OR_ID [--version VERSION]` | 安装目录中有权写入的版本并登记。渠道决定可用版本 |
| `mo app upgrade [NAME_OR_ID] [--version VERSION]` | 指定实例时更新该实例，只有这时可加 `--version`。省略目标时刷新索引并处理符合条件的升级。批量预演读取已有索引 |
| `mo app uninstall NAME_OR_ID` | 卸载选定实例，用户配置和数据按原生维护契约保留 |
| `mo app adopt [NAME_OR_ID]` 或 `mo app adopt --id ID` | 为已有安装记录身份和基线。省略目标在终端选择 |
| `mo app adopt --id ID --bind-recipe` | 核对与现有实例匹配、已获授权的维护配方。必须提供明确目标 |
| `mo app doctor [NAME_OR_ID]` | 核对本机证据。省略目标查看目录内可检查项 |
| `mo app discover [MANAGER]` | 发现本机安装，可限定管理器 |
| `mo app refresh [NAME_OR_ID]` | 刷新一个或全部软件的可用版本 |
| `mo app outdated [NAME_OR_ID]` | 从现有索引查可用版本 |
| `mo app service list`、`mo app service check [NAME]` | 找已声明的服务或检查状态 |
| `mo app start ID`、`mo app stop ID` | 在核验的用户范围启停已声明服务 |
| `mo app resources` | 返回应用资源快照，脚本可配合 `--json` |

```sh
mo app outdated jq
mo app upgrade jq --dry-run
mo app service list
```

原生渠道的查询、接管和写入资格各有范围。uv 只能观察和接管基线。mise 通用卸载要求目标为已登记、满足身份校验的单个非活动版本。Agent CLI 使用 [Agent 命令](agents.md) 查看固定版本及本机设置漂移。 [软件与服务流程](../flows/software.md) · [uv 观察](../flows/uv.md)
