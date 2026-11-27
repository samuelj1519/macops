# 开发、验证与交付

在仓库根目录使用 `rust-toolchain.toml` 指定的 Rust 与 `Cargo.lock`。Apple Silicon macOS 是 CI 和发布环境。按任务准备锁定依赖及适用的本地检查工具。 [项目约定](../../AGENTS.md)

## 从改动到验证

```sh
cargo build --locked
bash tools/check.sh quick
bash tools/check.sh tests
```

| 命令 | 检查内容 |
| --- | --- |
| `bash tools/check.sh quick` | Cargo 格式和严格 Clippy |
| `bash tools/check.sh tests` | 全目标测试和单独的文档测试 |
| `bash tools/check.sh quality` | 公开源码、工具测试、拼写、TOML、依赖和覆盖率 |
| `bash tools/check.sh full` | 以上检查和 Release 构建 |

全目标执行和覆盖率通过 `tools/run_ci_tests.sh` 并行运行两组 Cargo 测试。库、二进制、示例和基准目标使用 1 个线程，避免子进程继承其他单元测试持有的文件锁。集成测试使用 4 个线程。两组都结束后才汇报结果。

按改动选择实际验证。查看帮助和退出码时直接调用这次构建的二进制。验证软件写入时记录实际 argv、目录和回执。验证终端交互时通过 PTY 使用按键，检查确认目标、取消、窄屏、进度和最终状态。 [测试入口](../../crates/app/tests/cli.rs) · [终端测试](../../crates/app/tests/cli/browser_pty.py)

## 隔离与证据

- 使用独立源码和构建根、合成 HOME 与明确的管理器目录。测试假工具时构造确定的 PATH 和 fixture。涉及实际软件或账号时，先证实具备所需隔离与授权。`--home` 只改变逻辑作用域。
- 记录候选版本、二进制 SHA-256、执行命令、退出码以及原日志的位置。分开记录合成检查、实际原生工具验收、系统安装和远端验证的结论。
- 覆盖关键分支，例如预演无业务写入、错误输入、同名冲突、目录替换、并发、超时、未知结果和旧记录兼容。对关键保护可重放反例或运行验证脚本中的错误实现。
- 日志、覆盖率与构建输出放在独立临时目录或 Git 忽略的 `artifacts/`。结束后清理本任务可重建的临时结果，保留仍被依赖的恢复材料。

## 覆盖率和模型

[`quality-policy.json`](../../quality-policy.json) 定义文件分类和门槛。纯逻辑逐文件的行、函数和区域须达 100%。整体行和函数至少 90%，区域至少 85%。增添生产逻辑后核对分类，独立测试源码不纳入生产计数。

流程或状态契约变更时检查相应模型。模型验证的是声明的状态空间，实际文件、进程和运行环境要用适配器测试。使用 `TLA2TOOLS_JAR` 指定本地 JAR，部分验证器也接受 `--jar`。 [模型和命令](../models/README.md)

| 工具 | 使用场景 |
| --- | --- |
| [`check_coverage.py`](../../tools/check_coverage.py) | 检查覆盖率分类、门槛或指定基线 |
| [`check_public_source.py`](../../tools/check_public_source.py) | 检查 Git 跟踪源码中的凭据模式、个人路径和敏感文件类型，仅报告文件、行和类别 |
| [`verify_models.py`](../../tools/verify_models.py) | 检查脚本登记的 TLA+ 模型与可达条件 |
| [`verify_native_plugin_model.py`](../../tools/verify_native_plugin_model.py) | 检查 Codex 插件有限状态模型 |
| [`verify_chezmoi_release.py`](../../tools/verify_chezmoi_release.py) · [`verify_chezmoi_native_owner.py`](../../tools/verify_chezmoi_native_owner.py) | 在隔离范围验证给定二进制 |
| [`verify_protections.py`](../../tools/verify_protections.py) 与专题验证器 | 根据改动核对关键保护和对应错误实现 |

## 提交、集成与发布

用 `git config core.hooksPath .githooks` 启用项目钩子。提交时，钩子检查暂存差异的空白并执行 `quick`。非简单改动由独立审查者核对固定候选。 [代码审查](review.md)

获得集成授权后，从干净任务分支运行 `bash tools/integrate.sh HEAD`。脚本在独立源码和构建目录中采集当前 `main` 的覆盖率并完整检查候选。逐项达到基线后，重新核对工作区、原分支和主干，再快进集成。推送使用普通 Git 操作，按授权执行。 [集成脚本](../../tools/integrate.sh)

版本标签工作流复用相同提交的 `main` CI，要求 `checks` 和 `quality` 均成功，且标签提交可从 `origin/main` 到达。发布构建打包 Apple Silicon 二进制、MIT 许可证和 SHA-256。发布后可用 `python3 -B tools/release.py verify TAG --repo OWNER/REPO` 核验实际发布包。 [CI](../../.github/workflows/ci.yml) · [发布流程](../../.github/workflows/release.yml)
