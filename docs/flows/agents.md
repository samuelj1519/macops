# Agent 配置与 CLI

## 登记和选择已有配置

先用 `mo agent` 核对启动命令、配置目录和登记状态。发现不到自定配置时，用 `mo agent add NAME --config /absolute/path` 登记。程序核对可执行入口、客户端类型和目录。绑定保存后，即使对应的环境选择器不再存在，程序也能按记录查找。目录被替换或与绑定不一致时，需要重新核对。 [配置发现实现](../../crates/app/src/agent/resource_agents.rs) · [登记规则](../../crates/app/src/policy/profile/profile_registry.rs)

已有的正常配置用原命令启动。通过 macops 新建的单独配置有自己的原生 HOME。可用 `mo agent launch PROFILE` 启动，用 `mo agent uninstall PROFILE --dry-run` 查看移入归档的位置。启动前在预演中核对工作目录和插件偏好。原生程序及插件依赖还要能在该启动环境找到。 [Agent 命令](../commands/agents.md)

## 安装与升级 CLI

```sh
mo agent tools
mo agent upgrade codex --dry-run
```

先核对运行版、固定版和管理源声明。Agent CLI 根据各工具支持的渠道准备目标，再检查原配置、作用域和版本计划。`agent::ai` 识别工具并构造计划，`agent::ai_tx` 管理原生阶段及后置核验。mise 操作可能同时涉及运行版、本机 pin 和静态管理源。

| 分支 | 为什么会出现 | 下一步 |
| --- | --- | --- |
| 同一升级已经完成 | 重跑时核对原记录，载荷和版本已符合原目标 | 查看当前版本 |
| 本机 mise 配置与管理源有差异 | 新计划要求先明确两份配置的关系 | 用 `mo config reconcile` 看差异。明确只更新本机时先加 `--local --dry-run` |
| 原进程还存在 | 不能确定上一条命令停止写入 | 等它退出，再重试原操作 |
| 原结果待核对 | 原始载荷、版本、目录或配置证据不足 | 根据提示核对原范围和证据 |

Claude Code 升级还检查原 Claude 配置快照。其他工具按各自冻结条件核对。旧记录可以包含多余 Claude 快照，仍按原计划摘要和真实目标确定证据范围。原命令输出用于进度展示。进度和子进程退出都只是执行证据的一部分。

[升级入口](../../crates/app/src/agent/ai.rs) · [升级事务](../../crates/app/src/agent/ai_tx.rs) · [mise 终端进度](../../crates/app/src/terminal/mise_progress.rs) · [升级与恢复模型](../models/ai_upgrade/) · [原生 CLI 测试](../../crates/app/tests/cli/ai_native.rs)
