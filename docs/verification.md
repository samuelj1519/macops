# 开发验证

使用 `rust-toolchain.toml` 固定的 Rust 和 Cargo.lock。工作区继承 lint，禁止 unsafe。准备锁定依赖与本地 advisory 数据后运行：

```sh
bash tools/check.sh
```

完整检查包含格式、严格 Clippy、所有 target 测试、独立 doc tests、覆盖率校验器的负向测试、拼写、TOML、依赖及许可证、覆盖率和 Release 构建。CI 在 Apple Silicon macOS 上运行。提交使用仓库的 `.githooks/pre-commit`。需要时设置 `git config core.hooksPath .githooks`。

公开源码检查读取 Git 跟踪的文件，检查常见凭据模式、本机账号路径、个人邮件地址和敏感文件类型。结果只显示文件、行号及类别。版本标签在 CI 检查通过后打包 Apple Silicon 产物，内含 MIT 许可证和 SHA-256 校验值。

TLA+ 验证器可用 `TLA2TOOLS_JAR` 指定本地 JAR，默认读当前目录的 `tla2tools.jar`。带 `--jar` 的验证器也接受显式路径。

## 通过标准

- `quality-policy.json` 定义完整分类。纯逻辑每文件的行、函数和区域均须 100%。整体行和函数均须至少 90%，区域须至少 85%。生产计数排除独立测试源码，声明文件加入执行逻辑后须重新分类。
- 合成测试使用独立 HOME、源码和构建根。用已编译的 CLI 检查公开入口、退出码、实际文件、锁和恢复状态。`--home` 只改变逻辑范围，真实二进制与安装器需适用的系统隔离。
- 测试覆盖零写预演、错误输入、目录交换、同名实例冲突、超时、未知结果、旧 journal 和账本兼容。关键保护应有可重放的反例和可编译错误逻辑验证。
- 记录实际构建和运行的二进制摘要。合成检查、正式源码审查、原生工具验收和真实主机切换分别记录。当模型、流程或状态契约变化时更新并检查对应模型。 [状态模型](models/README.md)

## 复用工具

| 工具 | 用途 |
| --- | --- |
| `tools/check.sh`、`tools/check_coverage.py` | 完整工程门禁与覆盖率校验 |
| `tools/verify_models.py` 和对应专题验证器 | TLA+ 基线、可达状态及错误逻辑检查，需要本地 Java 和 TLC |
| `tools/verify_migration_release.py` | 用提供的 Release 二进制执行合成转换验收 |
| `tools/verify_chezmoi_release.py`、`tools/verify_chezmoi_native_owner.py` | 在隔离范围验证给定的 macops 与 chezmoi 二进制 |
| `tools/verify_*_mutants.py`、`tools/verify_protections.py` | 检查关键断言能识别指定错误实现 |

检查日志、覆盖率导出和临时构建结果放在 Git 忽略的 `artifacts/` 或独立临时目录。运行中的证据以及恢复需要的唯一材料单独保留。确认没有人继续依赖后再清理可重建产物。
