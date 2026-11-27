# 配置查询、应用和归属

同一个配置有两个需要区分的位置。管理源是 chezmoi 用来生成内容的文件。本机目标是应用设置时实际读取的文件。先用 `mo config source-path TARGET` 和 `mo config diff TARGET` 确认位置与变更。 [配置命令](../commands/configuration.md)

## 从登记到应用

| 阶段 | 行为与分支 |
| --- | --- |
| 登记 | `config enroll` 记录 native owner 及路径。管理器身份或路径变化时重新核对 |
| 查看 | `config list --refresh` 调用原生工具，建立受管路径清单。受过滤规则限制时报告部分覆盖。已有缓存和当前核验分别标识 |
| 预演 | `config apply TARGET --dry-run` 使用已有证据解析目标。缺少证据时报告待解析 |
| 实际应用 | 检查原生范围、管理源和实际目标。由 chezmoi 解释模板、过滤、配置与 hooks。写入后检查实际结果 |
| 恢复 | 部分应用或未知时按原记录检查。需要修复原生设置时使用前向操作。旧本地事务按原有备份及还原规则处理 |

在详情中可读原管理源和本机文件。浏览器只打开通过核验的普通 UTF-8 文件，大小限制为 256 KiB。查看与改动走各自的命令路径。原生查询也可能启动辅助进程，执行器用同一时限观察进程组。 [配置浏览器](../../crates/app/src/config/chezmoi_browser.rs) · [原生配置适配器](../../crates/app/src/config/chezmoi_native.rs)

## 登记现有文件和停止管理

`config adopt` 将已有普通文件纳入支持的静态管理源。它会在管理源中生成明文文件。确认前查看源路径和文件类型。`config forget` 移除选定的源条目，保留实际文件。当前原生源编辑要求受支持的 chezmoi 2.73.3、完整清单、已存在的源父目录和适用的默认状态目录。

执行时先在私有目录检查单文件原生操作，再核对目标目录身份、原内容与计划。成功发布后更新记录。涉及内容并发变化、目录被替换、清单不完整或上一次的原始证据缺失时，暂停或保留待核对状态。 [源文件事务](../../crates/app/src/config/chezmoi_source.rs) · [源操作验证](../../crates/app/tests/cli/chezmoi_source.rs) · [源操作模型](../models/control_plane/ChezmoiSource.tla)
