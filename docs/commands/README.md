# 命令参考

| 目标 | 入口 |
| --- | --- |
| 查本机、找安装方式、核对上次结果 | [查询、推荐和恢复](utility.md) |
| 安装、升级、卸载软件或操作服务 | [软件](software.md) |
| 安装、更新、启停技能与插件 | [技能与插件](skills.md) |
| 查询、登记、升级或启动 Agent | [Agent](agents.md) |
| 比较、应用或维护配置 | [配置](configuration.md) |
| 导出清单、管理目录、维护旧数据 | [高级维护](maintenance.md) |

`mo help` 查入口，`mo help skill install` 查子命令，`mo help advanced` 查高级参数。示例里的 `NAME`、`ID`、`OWNER/REPO` 和 `PATH` 都要替换为实际值。用 `mo app NAME` 等入口查对象详情。出现同名对象时，用详情里的唯一 ID。软件维护也支持 `mo app NAME upgrade` 的顺序。

## 全局参数

| 参数 | 默认或适用条件 | 用途 |
| --- | --- | --- |
| `--dry-run` | 省略时按命令正常执行 | 只看计划，不启动安装器或写入持久业务状态。依赖证据不足时会列出待解析项。部分预演仍会读取公开来源 |
| `--json` | 默认按终端环境选择人类可读输出 | 返回结构化结果，适合脚本。程序用文本呈现的列宽和颜色可以变化 |
| `--yes` | 默认按当前操作决定是否询问 | 跳过支持的确认，仍检查范围、证据和写入条件 |
| `--verbose` | 默认简洁输出，可重复传入 | 显示可用的原生诊断或推荐细节 |
| `--timeout SECONDS` | 120 秒，可取 1–3600 秒 | 为外部程序配置时限。进程组停止和结果核验仍须完成后才能安全收口 |
| `--home PATH` | 当前 HOME | 指定逻辑用户范围。涉及真实原生工具的测试需要系统隔离 |
| `--settings PATH` | `HOME/.local/settings` | 指定本机设置目录，显式路径须存在 |
| `--config-dir PATH`、`--data-dir PATH` | 按 XDG 与 HOME 解析 | 指定 macops 自有配置或持久数据目录 |
| `--state-dir PATH`、`--cache-dir PATH` | 按 XDG 与 HOME 解析 | 指定操作记录或缓存目录。恢复时要沿用原范围 |
| `--catalog PATH` | macops 配置目录的 `catalog.json` | 指定软件目录文件。显式路径缺失或文件损坏时报告错误 |
| `--chezmoi-source PATH`、`--chezmoi-destination PATH`、`--chezmoi-config PATH` | 适用于支持的配置命令 | 选择原生管理源、本机目标根及配置。见 [配置范围](configuration.md#配置范围) |
| `-h`、`--help`、`-V`、`--version` | 按需使用 | 显示帮助或版本 |

`mo app`、`mo config`、`mo skill`、`mo agent` 还接受 `--project /absolute/path`，用于项目清单、资源快照和详情查询。项目模式下请按对象名称或 ID 查询技能详情，技能列表的 `--details` 和 `--only` 只用于本机范围。

## 输出与退出码

| 退出码 | 含义 | 如何处理 |
| --- | --- | --- |
| 0 | 请求正常完成 | 仍要读结果项。例如预演和取消都可能正常退出 |
| 1 | 命令执行失败 | 读取错误原因和建议 |
| 2 | 命令用法错误 | 查对应子命令的帮助 |
| 3 | 结果需要处理 | 读取不完整项、原操作状态、待续步骤或未执行目标 |

业务 JSON 包装包含 `schemaVersion`、`status`、`data` 和 `warnings`，执行错误使用 `error`。`status` 可以是 `ok`、`attention` 或 `failed`。语法错误在业务分派前处理。有些查询还在 `data` 中报告覆盖情况和 `incomplete`。脚本应该按所调用命令的结果字段与进程退出码处理。 [输出实现](../../crates/app/src/lib.rs)
