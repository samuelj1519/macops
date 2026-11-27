# 原生插件配方参考

使用 `mo skill plugins list` 查看声明的客户端实例。用 `--instance claude:mi` 等标签筛选。Claude 的实例标签是 `claude:default`、`claude:mi`、`claude:kimi`、`claude:glm` 和 `claude:zen`，另外可声明 `codex` 和 `cursor`。每个实例单独绑定绝对路径和二进制摘要。Claude 的 `plugin_root` 可对应 `CLAUDE_CODE_PLUGIN_CACHE_DIR`，缺省为 `<config_root>/plugins`。

在 macops 配置目录保存 `native-plugin-recipes.json`。下面的路径是占位符。路径须指向实际文件或目录，并通过不跟随符号链接的核对。使用 `mo dirs --roots` 查 macops 配置目录。每个实例的 `home` 须与本次 macops 的 HOME 相同。

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

用 `mo skill plugins check my-plugin --json` 读取本地登记、配置中的启用值与可核对载荷。配置启用、实际载荷、有效启用和会话载入分开报告。对照固定的文件内容时可加 `source.sha256`，填写该子树的摘要。

用 `mo --dry-run skill plugins upgrade my-plugin` 预演。当前的写入合同只覆盖已验证的 ARM64 Codex 0.169.0 单插件同源同版本重装。目标必须已有本地 marketplace 登记、配置明确启用和可核对的安装缓存。源和选定缓存使用验证过的清单格式，执行目录避开项目配置层，存在凭据文件或共享存储时阻断。符合条件后执行 `mo skill plugins upgrade my-plugin`，非交互环境用 `--yes` 确认。

命令通过 Codex 的 `plugin add` 执行。macops 在启动前冻结作用域与投影，在原进程结束后核对插件、来源、配置和无关插件缓存。先用 `mo recover ID` 查询 Unknown 操作。收口需要原进程退出、原后验记录和持锁重读的证据一致。

公开仓库中新装插件走推荐或能力安装入口。两套入口有各自的来源和客户端验证条件。 [推荐与插件安装](../flows/recommend.md)
