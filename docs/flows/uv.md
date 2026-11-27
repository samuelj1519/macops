# uv 工具的只读观察与接管

macops 从已有 uv 工具目录读取 receipt、包元数据和入口印记。`mo app`、`mo app NAME_OR_ID`、`mo app doctor`、`mo app discover uv` 和接管预演不运行 uv、Python 或工具入口，也不创建原生锁、工具目录、state 或 cache。

## 查询与接管

先运行 `mo app resources --json`，从结果复制完整 ResourceID。不同工具根中的同名包具有不同 ID。包名按 PEP 503 子集归一化，例如 `Demo_Tool` 与 `demo-tool` 表示同一根中的同一包。

```sh
mo app '<ResourceID>'
mo app adopt --id '<ResourceID>' --dry-run
mo app adopt --id '<ResourceID>' --yes
```

接管只向所选 macops state 写入观察基线和原 operation ID，不授予 uv 写权限。接管后的 `install`、`upgrade`、`uninstall` 和 `refresh` 仍返回 `uv_write_blocked_review`。当前没有 uv authority 字段，也不自动改成 command provider 执行。

已有 `uv` 声明需要明确绑定规范绝对路径，才能接管：

```json
{
  "type": "uv",
  "package": "demo-tool",
  "binding": {
    "tool_dir": "/absolute/uv/tools",
    "bin_dir": "/absolute/tool-bin"
  }
}
```

路径的祖先目录和工具环境目录必须是实际目录，不能通过符号链接跳转。入口可为受约束的符号链接；数据文件不能为符号链接或特殊文件。缺少 binding 的旧声明保留原序列化形状，以 `legacy_uv_query_unbound` 展示，不调用 `uv tool list` 或 `uv tool dir`。

同一规范实例只允许一个 owner。唯一已声明或已接管的 owner 会沿用原 ID；不同 ID 的声明与收据冲突时，两种 ID 入口都会拒绝接管。旧未绑定收据或跨 provider 表示迁移返回 `owner_migration_required`，不会自动移植或删除原记录。

## 根与观察范围

已知根来自显式 binding、UV 环境路径、已接管查询及固定 Unix 默认规则；不是全盘发现。默认工具根为 `UV_TOOL_DIR`，否则为 `HOME/.local/share/uv/tools`。没有显式 `UV_TOOL_DIR` 且存在 XDG data/state 覆盖时，默认根报告 unavailable，可用 binding 补齐。

binDir 依次取 `UV_TOOL_BIN_DIR`、`XDG_BIN_HOME`、`XDG_DATA_HOME` 的父目录下 `bin`、`HOME/.local/bin`。隔离 HOME fixture 不继承宿主覆盖。每个根单独报告覆盖状态；读取失败、布局歧义、超预算和检测到的变动都不能生成可接管的 Present。

读取范围限于支持的 Unix 布局和 receipt 中的入口。每根最多 4096 个工具名，现存 `.lock` 不占工具名额，每个 site-packages 最多 4096 项，receipt 最多 128 个入口；receipt、METADATA 和单脚本各限 1 MiB，pyvenv.cfg 限 64 KiB，每次目标观察累计读取限 8 MiB。超限时返回 Unknown，不截断后接管。解释器链接只记录文本，不读取其目标。

已接管的工具根或 binDir 缺失、身份改变时，`discover uv` 与其他查询均沿用收据中的原 namespace，保留 Unknown 行并将该根的覆盖状态标为 invalid；不重新接管替换后的目录，也不推断卸载成功。未接管对象继续从当前支持的本地目录发现。基线比较覆盖已读元数据、入口和脚本；不认证来源、基础解释器或整个依赖环境。前后印记和现存锁的共享非阻塞检查不能提供外部恶意 writer 的原子快照保证。

## 恢复

接管恢复沿用原 journal、query、owner、receipt 和 operation ID，不重新运行安装器。已提交收据必须与冻结输入一致，观察或 owner 变化时保留阻断。未提交且未发送原生 gate 的 Prepared 操作可按既有恢复规则记录失败。

旧未绑定 uv 操作的普通 `recover` 只读返回 `legacy_uv_query_unbound`，不执行旧查询，也不把旧操作收口为成功。人工 `mark-failed` 保留原有确认合同。

uv 集成目前支持本地观察和接管。原生生命周期、共享入口写入、下载凭据隔离及最后一个工具的清理均未开放。

## 合成验收

验收脚本要求显式指定本次构建的二进制文件，并在结果中记录其 SHA-256。使用隔离的合成 HOME，不调用真实 uv 或工具入口。

```sh
cargo build --release --locked --offline
python3 tools/verify_uv_release.py --binary target/release/macops
python3 tools/verify_uv_discovery.py --binary target/release/macops --output artifacts/uv-readonly/discovery-results.json
```

设置了 `CARGO_TARGET_DIR` 时，将 `--binary` 改为该构建目录下的 `release/macops`。

[uv 观察实现](../../crates/app/src/software/uv_local.rs) · [身份与归属](../../crates/app/src/software/uv_ownership.rs) · [观察模型](../models/uv_observed/UvObserved.tla)
