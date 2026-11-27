# macops

macops 是面向 macOS 的 Rust 命令行工具。用 `mo` 查看和维护软件、Agent、技能、插件与配置。它负责识别目标、组织操作和核对结果，实际安装和配置应用由相应的原生工具执行。

## 安装

在 Apple Silicon Mac 上，从源码目录构建并安装当前代码。构建使用仓库锁定的 Rust 工具链。

```sh
./install.sh
```

已有安装脚本时，可以直接安装最新正式发布版。脚本会核对发布包的 SHA-256，发布模式无需 Rust。

```sh
sh install.sh --release
```

安装目标是 `~/.local/libexec/macops`。脚本会验证 `~/.local/bin` 中的 `mo` 和 `macops` 两个入口。升级时重新执行相应的安装命令。 [正式发布](https://github.com/samuelj1519/macops/releases)

## 开始使用

```sh
mo
mo recommend jq
mo skill install owner/repo --dry-run
mo help skill install
```

第一条命令打开本机清单。第二条查安装方式。第三条预览公开仓库的技能安装，使用前将 `owner/repo` 换成真实仓库名。最后一条显示安装命令的完整用法。

从 [项目文档](docs/README.md) 开始。也可直接查阅 [命令参考](docs/commands/README.md)、[架构与模块](docs/design/architecture.md) 或 [代码审查指南](docs/development/review.md)。

## 许可证

[MIT](LICENSE)
