# 软件和服务的原生维护

`mo app` 查询软件实例。可用版本查询走索引，安装、升级与卸载走原生管理器。先找到一份明确的安装，再按该渠道允许的范围预演。 [软件命令](../commands/software.md)

| 任务 | 实现与分支 |
| --- | --- |
| 发现和接管 | `catalog` 汇总观察、软件目录和回执。同名不同安装范围保留独立身份。接管已有安装记录基线，维护权限按来源证据检查 |
| 刷新和查看版本 | `catalog::updates` 写入查询结果与索引，`outdated` 使用已有索引。索引缺失、过期或未知分别显示 |
| 安装和升级 | `lib::run_application` 等入口解析具体实例和版本，由原生适配器准备计划。确认后重新校验目标。精确版本要求由渠道决定 |
| 批量升级 | 先按索引筛选。需要显式版本的项单独报告。遇到需要停止的执行错误或不确定结果时保留后续未执行项 |
| 启停服务 | `software::service` 解析已声明服务，核对用户域、工具身份和原状态，再调用原生命令并复查 |

[`lib.rs`](../../crates/app/src/lib.rs) · [版本索引](../../crates/app/src/catalog/updates.rs) · [服务实现](../../crates/app/src/software/service.rs)

## 渠道的限制

| 渠道 | 核验边界 |
| --- | --- |
| Homebrew | 绑定 executable、prefix、Cellar 和完整 formula 名。预演列原生公式、版本和安装范围 |
| npm | 绑定 npm prefix、包名、manifest 和适用的 npm 配置。只读接管与绑定已授权维护配方是不同的操作 |
| mise | 绑定实际安装与配置目录。写入前核对目录身份与 chezmoi 归属。通用卸载仅覆盖已核验的非活动单个版本 |
| Bun、Cargo、命令配方 | 按已声明配方与观测范围调用实际程序。命令配方使用程序名与独立参数 |
| uv | 观察工具文件与基线。uv 生命周期由原生工具处理 |

本机默认目录中的 `catalog.json` 使用版本 1。显式目录文件可通过 `--catalog` 指定。使用命令型配方时按本机脚本审查。查询输出使用带 `status` 的对象。`present` 带版本，`missing` 表示已核验缺失，`unknown` 带原因。

[Homebrew 适配器](../../crates/app/src/software/brew.rs) · [npm 接管](../../crates/app/src/software/npm_adoption.rs) · [mise 卸载](../../crates/app/src/software/mise_retirement.rs) · [uv 观察](uv.md) · [原生维护模型](../models/README.md)
