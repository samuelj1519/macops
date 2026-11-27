# macops 项目文档

这套文档适合第一次接触 macops 的使用者和开发者。先学会定位一个本机对象，再跟着一次真实操作读到负责它的 Rust 代码。准备修改代码时，用相同的流程和证据完成审查。

| 阅读顺序 | 阅读入口 | 读完可以做什么 |
| --- | --- | --- |
| 1 | [第一次使用](start/first-run.md) | 打开本机清单，区分软件、Agent、能力和配置，安全预演操作 |
| 2 | [命令参考](commands/README.md) | 按目标找到命令，查参数、默认行为、作用域和退出码 |
| 3 | [产品与核心概念](design/concepts.md) · [架构与模块](design/architecture.md) | 解释原生工具与 macops 分别负责什么，定位业务代码和持久状态 |
| 4 | [业务流程](flows/README.md) | 从命令跟到写入、核验和恢复，理解成功及各条异常路径 |
| 5 | [开发与验证](development/verification.md) · [代码审查](development/review.md) | 构建和验证实际 CLI，审查扩展、写入边界、重复执行与性能 |
| 按需 | [状态模型](models/README.md) | 找到可执行规范、检查命令和模型范围 |

建议先读通一条完整路径。假设两个 Claude Code 配置都装了同一来源的插件，先看 [如何预演升级](start/first-run.md#预演两个-agent-的能力升级)，再查 [Agent 选择与共享范围](commands/skills.md#选择-agent-和写入范围)，接着读 [批量升级各分支](flows/capabilities.md#按-agent-升级)，最后用 [审查示例](development/review.md#演练审查一次批量升级改动) 检查一个可能写错的改动。

命令参考以当前源码构建的 `mo` 为准。使用已安装的发布版时，先运行 `mo --version` 和 `mo help 命令 子命令` 核对该版本支持的用法。
