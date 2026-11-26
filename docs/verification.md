# 开发验证

使用 `rust-toolchain.toml` 固定的 Rust 和 `Cargo.lock`。准备锁定依赖和本地 advisory 数据后，可以按阶段运行检查。

```sh
bash tools/check.sh quick    # 格式和严格 Clippy
bash tools/check.sh tests    # 全目标和独立文档测试
bash tools/check.sh quality  # 源码、工具、依赖和覆盖率
bash tools/check.sh full     # 完整检查和 Release 构建
```

任务分支的 `.githooks/pre-commit` 检查暂存差异的空白，并对当前工作区执行 `quick`。需要时用 `git config core.hooksPath .githooks` 启用钩子。提交前检查和合入验证使用不同输入，合入验证以明确指定的提交为准。

从干净的任务分支运行 `bash tools/integrate.sh HEAD`。脚本在独立源码和构建目录中采集当前 `main` 的覆盖率，并对候选执行一次完整检查。覆盖率逐项达到基线后，脚本再次核对原分支、主干提交和工作区状态，再切换到 `main` 并快进合入。覆盖率采集会清理旧 profile。推送通过普通 Git 命令完成。

全目标测试和覆盖率各使用 `tools/run_ci_tests.sh` 并行运行两组 Cargo 测试。库、二进制、示例和基准目标使用 1 个线程，集成测试使用 4 个线程。单元测试串行执行，避免派生子进程继承其他测试持有的文件锁。两组都结束后才汇报结果。

公开源码检查读取 Git 跟踪的文件，检查常见凭据模式、本机账号路径、个人邮件地址和敏感文件类型。结果只显示文件、行号及类别。

CI 在 Apple Silicon macOS 上运行。版本标签复用同仓库、同 SHA 的 `main` 推送 CI，要求 `checks` 和 `quality` 均已成功。发布任务核对标签提交能从 `origin/main` 到达，再打包 Apple Silicon 二进制、MIT 许可证和 SHA-256 校验值。发布后用 `python3 -B tools/release.py verify v0.4.1 --repo OWNER/REPO` 下载验收。将示例标签和仓库名换成实际值。

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
| `tools/verify_models.py` | 已登记的 TLA+ 基线与可达状态检查 |
| `tools/verify_uv_model.py`、`tools/verify_native_skills_model.py` 等专题验证器 | 对应功能的模型与错误逻辑验证 |
| `tools/verify_native_plugin_model.py` | 使用 Python 检查 Codex 插件的有限状态模型 |
| `tools/verify_chezmoi_release.py`、`tools/verify_chezmoi_native_owner.py` | 在隔离范围验证给定的 macops 与 chezmoi 二进制 |
| `tools/verify_*_mutants.py`、`tools/verify_protections.py` | 检查关键断言能识别指定错误实现 |

检查日志、覆盖率导出和临时构建结果放在 Git 忽略的 `artifacts/` 或独立临时目录。运行中的证据以及恢复需要的唯一材料单独保留。确认没有人继续依赖后再清理可重建产物。
