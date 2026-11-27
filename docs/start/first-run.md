# 第一次使用 macops

macops 把本机的软件、Agent、技能、插件和配置放到一个入口。它会告诉你找到了什么、将操作哪一项，以及原生工具执行后能否确认结果。

## 查到本机对象

在 Apple Silicon Mac 上按 [安装说明](../../README.md#安装) 准备工具，然后运行：

```sh
mo --version
mo
```

| 入口 | 要找的对象 | 先核对什么 |
| --- | --- | --- |
| `mo app` | jq、ripgrep 等软件 | 安装渠道、作用域、版本。相同软件名可能对应多份安装 |
| `mo agent` | Codex、Claude Code、Cursor 等 Agent | 启动命令和配置目录。同一个程序可以服务多套配置 |
| `mo skill` | Agent 可用的技能与插件 | 部署位置、可读取它的 Agent、登记和启用证据 |
| `mo config` | chezmoi 管理的配置 | 管理源和本机文件各自的位置 |
| `mo history` | 近期维护操作 | 目标、结果和是否仍有步骤需要处理 |

在清单中用方向键移动、`Tab` 切换类别、`/` 搜索、`Enter` 查看详情、`Esc` 返回、`q` 退出。可用操作显示在页脚。只知道名称时用 `mo search NAME`，找到结果后复制详情里的 ID，便于精确指定实例。

软件清单顶部列出安装来源，能力清单顶部列出安装位置。按 `←` 或 `→` 切换，方括号标出当前选项。选「全部」可清除筛选。窄窗口中标签分行显示，搜索词仍有自己的行。

在详情中，按页脚显示的键操作当前选中项。快捷键随安装来源、管理状态和配置归属变化。

| 类型 | 常用详情操作 |
| --- | --- |
| 软件 | `i` 安装，`u` 升级，`x` 卸载，`a` 检查接管条件。维护时调用对应的安装工具和原安装范围。仅盘点的条目可在详情核对渠道与范围 |
| 能力 | `u` 升级、`x` 卸载符合条件的共享技能和插件。Claude Code 插件从原市场升级。符合固定配方的 Codex 插件可以重装。支持启停的插件显示 `e` 启用和 `d` 停用 |
| Agent | 在 Agent 清单按 `r` 读取 CLI 版本，选择 CLI 行后可用 `u` 升级。`x` 可归档 mo 创建的单独配置，或停止接管已登记的外部配置 |
| 配置 | `d` 查看差异，`v` 核验，`p` 应用。来源和归属支持时，可用 `x` 停止管理并保留本机文件 |

## 找安装和维护入口

运行 `mo recommend jq` 查看软件安装方式，或把目标换成公开仓库名称 `OWNER/REPO`。推荐页先列本机发现，再给出可执行的候选和文档指引。选择候选后核对来源、版本和实际写入位置，再确认操作。已有安装时，先看维护入口和现有来源。 [推荐与安装](../flows/recommend.md)

## 预演两个 Agent 的能力升级

假设已在两套 Claude Code 配置中安装同一来源的能力。把 `owner/repo` 和 Agent 名换成 `mo skill --details` 与 `mo agent` 中实际显示的值。

```sh
mo skill upgrade owner/repo --agent claude-mi --agent claude-glm --dry-run
```

核对每行的 Agent、安装位置、现有版本与目标版本。遇到被阻止的目标时先读该行原因。确定影响范围后去掉 `--dry-run` 执行。终端未指定 Agent 时，受支持的维护流程会提供多选。安装还涉及共享目录时，确认前核对能读取同一目录的其他 Agent。 [能力命令](../commands/skills.md)

执行后，如果某行显示「结果待核对」，后续目标可能显示「尚未开始」。原命令重试时会先尝试核对适用的旧操作。用 `mo recover` 可以查看待处理记录。 [处理未完成的操作](../flows/recovery.md)

## 在隔离目录试读命令

在仓库根目录构建当前代码，再用空 HOME 和最小 PATH 练习查看、预演和核对。脚本在子 shell 中创建并清理练习目录。前四次查询分别显示版本、命令用法、实际目录和结构化能力清单。空目录中没有真实 Agent，清单可以为空。

```sh
cargo build --locked
(
  set -e
  lab=$(mktemp -d)
  trap 'rmdir "$lab/home" && rmdir "$lab"' EXIT
  mkdir "$lab/home"
  binary="$PWD/target/debug/macops"
  env -i HOME="$lab/home" PATH=/usr/bin:/bin "$binary" --version
  env -i HOME="$lab/home" PATH=/usr/bin:/bin "$binary" help skill upgrade
  env -i HOME="$lab/home" PATH=/usr/bin:/bin "$binary" dirs --roots
  env -i HOME="$lab/home" PATH=/usr/bin:/bin "$binary" skill --json
  env -i HOME="$lab/home" PATH=/usr/bin:/bin "$binary" config apply /absolute/example --dry-run
  env -i HOME="$lab/home" PATH=/usr/bin:/bin "$binary" recover
)
```

配置预演会提示目标仍待解析，因为练习目录里没有配置登记和清单。最后的核对应该显示没有待检查的操作。若设置了 Cargo 构建目录，按实际位置调整 `binary`。涉及真实原生安装器的测试需遵守 [开发验证中的隔离要求](../development/verification.md#隔离与证据)。
