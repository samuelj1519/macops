# 原生插件安装与维护

## 独立档案安装

使用 `mo plugins` 查看 Codex、Claude Code 和 Cursor 的市场。对支持的公开仓库，`mo recommend OWNER/REPO` 会展示绑定到固定提交的安装候选。Apple Silicon 上可通过已验证的 Codex 0.170.0 或 Claude Code 2.1.328 安装到独立档案。

1. 在终端选择安装候选，核对仓库、提交、插件和目标档案。
2. 输入 `yes` 安装并启用插件，或输入 `disabled` 安装并停用插件。核验并登记成功后，档案会出现在 `mo agents`。
3. 在档案详情按 `e` 设置 macops 下次启动时的启停偏好，按 `l` 在当前工作目录启动。直接启动原生程序时使用档案自身的设置。

如果结果等待核对，运行 `mo recover OP-ID` 查看原操作。Claude Code 原操作已核验，且下一阶段尚未启动时，可以按提示使用 `mo resume OP-ID` 确认续装。

Cursor 的账户市场交给原生 `/plugin` 界面安装。使用 `mo list --kind capabilities --project /absolute/path` 查看指定项目中的 Cursor 插件声明。

## 已登记插件的同源重装

使用 `macops skills plugins list` 查看声明的客户端实例。用 `--instance claude:mi` 等标签筛选。Claude 的实例标签是 `claude:default`、`claude:mi`、`claude:kimi`、`claude:glm` 和 `claude:zen`，另外可声明 `codex` 和 `cursor`。每个实例单独绑定绝对路径和二进制摘要。Claude 的 `plugin_root` 可对应 `CLAUDE_CODE_PLUGIN_CACHE_DIR`，缺省为 `<config_root>/plugins`。

在 macops 配置目录保存 `native-plugin-recipes.json`。下面的路径是占位符。路径须指向实际文件或目录，并通过不跟随符号链接的核对。使用 `macops dirs --roots` 查 macops 配置目录。每个实例的 `home` 须与本次 macops 的 HOME 相同。

```json
{
  "schema_version": 1,
  "instances": {
    "codex": {
      "home": "/absolute/home",
      "config_root": "/absolute/home/.codex",
      "cwd": "/absolute/local-plugin-work",
      "binary": "/absolute/official-codex",
      "binary_sha256": "739cda86e15d30a6e2b40cf8dcbbfb988aaf48b305b27065573bb8e9fd41d002",
      "version": "0.169.0"
    }
  },
  "recipes": {
    "my-plugin": {
      "revision": "r1",
      "instances": ["codex"],
      "marketplace": "local-market",
      "plugin": "my-plugin",
      "source": {
        "type": "local_directory",
        "root": "/absolute/market",
        "member": "plugins/my-plugin"
      }
    }
  }
}
```

用 `macops skills plugins check my-plugin --json` 读取本地登记、配置中的启用值与可核对载荷。配置启用、实际载荷、有效启用和会话载入分开报告。对照固定的文件内容时可加 `source.sha256`，填写该子树的摘要。

用 `macops --dry-run skills plugins upgrade my-plugin` 预演。当前的写入合同只覆盖已验证的 ARM64 Codex 0.169.0 单插件同源同版本重装。目标必须已有本地 marketplace 登记、配置明确启用和可核对的安装缓存。源和选定缓存使用验证过的清单格式，执行目录避开项目配置层，存在凭据文件或共享存储时阻断。符合条件后执行 `macops skills plugins upgrade my-plugin`，非交互环境用 `--yes` 确认。

命令通过 Codex 的 `plugin add` 执行。macops 在启动前冻结作用域与投影，在原进程结束后核对插件、来源、配置和无关插件缓存。先用 `macops recover OP-ID` 查询 Unknown 操作。收口需要原进程退出、原后验记录和持锁重读的证据一致。
