# 从旧 mo 转换到 macops

适用于原先由 `~/.local/bin/mo`、`~/.local/bin/macops` 和 `~/.agents/bin/skillctl` 操作旧 settings 与治理数据的主机。转换是一次性操作。先用新二进制预检，转换通过后再发布新命令。

## 数据归属

| 类别 | 转换行为 |
| --- | --- |
| 应用清单、技能档案、资产声明、调用策略声明、来源渠道 | 校验后复制到 macops 配置目录下的 manifests |
| 受管资产基线、调用策略覆盖记录 | 校验后复制到 macops 状态目录 |
| 旧受管更新索引 | 复制到 macops 缓存目录，之后可按新版本索引重建 |
| 旧 settings、重建和统一治理历史、已有备份 | 在原位建立档案索引，为校验和恢复提供证据 |
| 第三方配置、技能和安装载荷、已登记的原生状态 | 归现有程序管理并原位保留，包括独立的 `billion-context` 状态 |
| 无法核对归属、冲突或被替换的目录 | 在预检的 `blocked` 列表报告，暂停转换 |

默认新目录为 `~/.config/macops`、`~/.local/share/macops`、`~/.local/state/macops` 和 `~/.cache/macops`。使用 XDG 覆盖时，预检、执行、验证和恢复沿用同一持久根。已发布账本的缓存根可按缓存语义重新建立。

## 预检和切换

1. 备份旧的启动器、有效 chezmoi 恢复项及其权限。查询当前源和启动途径。安排维护窗口，暂停能重新启动旧 writer 的任务，并确认运行实例退出。
2. 将旧入口与已知恢复项一并停用。支持的维护文件内容为 `#!/bin/sh\nexit 75\n`，live 模式为 `0700`，已管理的源模式为 `0600`。支持入口及其恢复项均不存在的布局。实际预检还检查 `~/.agents/backups` 根目录为 `0700`。
3. 用已核验的二进制预检。全部条件就绪后运行转换并验证。未知或冲突项须先落实归属。
4. 验证通过后发布新入口。可将单个二进制放在 `~/.local/libexec/macops`，由 `~/.local/bin/mo`、`~/.local/bin/macops` 及相应 chezmoi 源指向它。核对两条命令和当前治理账本后退役旧 `skillctl` 与确认可重建的旧缓存。

```sh
target/release/macops --json migrate preflight
target/release/macops migrate run
target/release/macops --json migrate verify
```

## 失败与恢复

迁移在落位和提交前再次核对停写与旧源。发布账本前仍以旧源为权威。发布后以新根为权威，旧源继续提供核验依据。若提交后的清理中断，保留维护状态。用原 ID 执行 `migrate recover ID` 并再次 `migrate verify`。先核实已发布账本的状态，再决定如何恢复入口。

旧档案若被改动，`migrate verify` 会报告分叉。清理旧入口和孤立缓存时，应保留已索引的 settings、旧治理数据、历史操作及回滚材料。客户端插件、原生数据和共享缓存继续按各自的来源维护。

## 命令对应

| 原用法 | 当前入口 |
| --- | --- |
| `--apply` 或旧预演模式 | 写命令默认执行，使用 `--dry-run` 预演 |
| `app list/info/check` | `list --kind applications`、`info`、`doctor` |
| `app upgrade` | 已支持的 AI CLI 路由，可直接用 `ai upgrade` |
| `toolchain ls/info` | `toolchain list/info` 或 `discover` |
| `skills sync` | 用技能、来源或 `profile:NAME` 选择投放，固定版本交付使用 `skills upgrade` |
| `config apply/restore` | 用 `config apply TARGET` 调用 chezmoi。旧本地事务在满足原证据时仍可恢复 |
