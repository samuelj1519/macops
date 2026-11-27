# 推荐、安装和模型设置

运行 `mo recommend NAME` 查软件，或使用 `mo recommend OWNER/REPO` 查技能和插件。页面先给本机状态，再列安装方案、来源文档与可用的维护入口。明确来源后才能预演可执行候选。 [推荐参数](../commands/utility.md#推荐参数)

## 从公开来源到安装计划

1. `recommend` 从内置证据、本机证据或选中的公开仓库读取说明。短名称查不到登记候选时，终端可搜索公开仓库。选中仓库后，将 README 和 INSTALL 指向同一个固定提交。
2. 程序查找已登记配方、支持的技能来源与原生市场清单。匹配要求满足时生成候选。公开说明仍可作为手动操作的依据。仓库的 README 或模型生成的命令只作为建议。
3. 用户核对来源、版本、实际目标及启用方式。确认控件用方向键与 Enter，Esc 返回。需要填写版本、密钥或路径时才输入文本。
4. 适配器在执行前复查证据。完成后重新读原生结果和目标，再登记成功或提示需要继续处理。

软件候选按渠道核对实际身份。短名称查找可对照 npm 或 Homebrew 的发布证据。共享技能安装要检查目录影响范围。原生插件计划固定市场与插件载荷，并由对应客户端安装。 [候选规则](../../crates/app/src/recommend/candidate.rs) · [上游证据](../../crates/app/src/recommend/upstream.rs) · [推荐模型](../models/control_plane/RecommendJourney.tla)

## 安装到已有 Agent 配置

以 Claude Code 为例，选择插件后按配置名称和目录选一套或多套兼容配置。多选时用空格标记，按 Enter 确认选择。尚未登记的配置会在安装确认中说明登记与写入。新建单独配置是单独的选项。核对选中的目标后，选择「安装并启用」或「安装后保持停用」。

[`agent::plugin_install`](../../crates/app/src/agent/plugin_install.rs) 负责选择与确认，[`profile_install`](../../crates/app/src/agent/profile_install.rs) 负责已有配置绑定与逐项安装。原生安装可以先在隔离目录生成载荷，[`plugin_target`](../../crates/app/src/agent/plugin_target.rs) 核验后再发布到用户选择的配置。客户端输出中的阶段路径可能指向隔离目录，最终结果中的目标配置才是用户使用的位置。程序核对目录身份，限制改动到插件树和归属的配置字段。已有配置安装后用平时的命令启动。

推荐菜单的多配置插件安装逐项登记和执行。首个失败或待核对的目标会结束这一批，菜单会列出尚未开始的配置。已完成的安装继续保留，本次尚未执行后续目标的登记或安装。按 Agent 的维护批次另有独立目标的失败处理。 [能力批量流程](capabilities.md#按-agent-升级)

| 看到的结果 | 下一步 |
| --- | --- |
| 有候选且核验完成 | 核对最终路径和启用设置，启动对应 Agent |
| 只有文档支持的安装方式 | 使用手动安装命令与上游说明 |
| 第一步核验完成，后面仍需执行 | 按结果给出的续装入口继续 |
| 旧记录待核对 | 在原作用域先尝试核对，同一次操作已经完成时可复用其结果 |
| 来源不可查询 | 查看仓库名称、访问条件或查询额度，再按提示重试 |

原生插件安装支持范围由版本、平台和二进制证据决定。公开来源的新装与已登记配方同源重装分别核对各自的原生条件。 [Codex 适配器](../../crates/app/src/agent/codex_install.rs) · [Claude Code 适配器](../../crates/app/src/agent/claude_install.rs) · [安装目标模型](../models/agent_install/AgentInstallTarget.tla) · [操作恢复](recovery.md)

## 推荐模型设置

用 `mo recommend --setup` 在终端选本地分析、DeepSeek 或自定义 HTTPS OpenAI 兼容服务。输入密钥时不回显。连接测试和保存各有确认。已有配置可以测试密钥、更换模型或更换服务。单改同一服务的模型可以使用已保存的密钥。

远端辅助只接收允许的公开文档摘录与工具可用情况。回答用来解释证据，安装资格由本地规则决定。凭据打不开、请求失败或回答无法核对时，仓库推荐可继续本地分析。日常读取不会主动请求钥匙串授权，设置向导可用于显式授权。 [模型使用规则](../../crates/app/src/recommend/model_policy.rs) · [模型边界](../models/control_plane/ModelAssist.tla)

对同一系统用户，推荐设置使用固定的 `org.macops.recommend.api-key` 条目。换服务时更新绑定与密钥。读不到已保存凭据时进入设置选 `t` 测试。上一次保存中断时选 `c` 核对原操作。向导还支持绑定现有通用密码条目，需要钥匙串文件绝对路径、服务和账户。外部条目变动后重新绑定。旧条目清理证据不足时可以按提示保留该条目。

保存和恢复用系统用户主目录下的 `~/.config/macops/recommend-model.lock` 串行化。自定义配置目录仍共用该用户的锁。推荐模型设置和软件维护拥有各自的状态。实际安装仍要遵守所用维护事务的恢复条件。 [设置事务](../../crates/app/src/recommend/setup.rs) · [钥匙串适配器](../../crates/app/src/recommend/keychain.rs)
