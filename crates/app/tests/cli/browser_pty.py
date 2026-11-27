"""Drive the compiled CLI on a disposable terminal using synthetic profiles."""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time
import unicodedata

scenario, target, width, exit_code, *command = sys.argv[1:]
reader, writer = pty.openpty()
height = 9 if scenario == "skill-short-location-groups" else 28
fcntl.ioctl(writer, termios.TIOCSWINSZ, struct.pack("HHHH", height, int(width), 0, 0))
original = termios.tcgetattr(writer)
capture = bytearray()


environment = dict(os.environ, TERM="xterm-256color")
if scenario == "skill-adopt-style":
    environment.pop("NO_COLOR", None)
elif scenario == "skill-adopt-no-color":
    environment["NO_COLOR"] = "1"
elif scenario == "app-version-evidence":
    environment["PATH"] = f'{Path(target) / ".local/bin"}:{environment["PATH"]}'
process = subprocess.Popen(
    command,
    stdin=writer,
    stdout=writer,
    stderr=writer,
    env=environment,
)


def receive():
    try:
        capture.extend(os.read(reader, 65536))
    except OSError as error:
        if error.errno != errno.EIO:
            raise


def expect(text, offset=0):
    def contains():
        segment = bytes(capture[offset:])
        if text.startswith("\x1b"):
            return text.encode() in segment
        plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", segment).decode(errors="replace")
        return "".join(text.split()) in "".join(plain.split())

    limit = time.monotonic() + 12
    while not contains() and time.monotonic() < limit:
        if select.select([reader], [], [], 0.1)[0]:
            receive()
        elif process.poll() is not None:
            break
    assert contains(), f"{text!r} was absent from {bytes(capture[-2000:])!r}"


def type_key(key):
    offset = len(capture)
    os.write(reader, key)
    return offset


def screen_cells(number):
    cells = [" "] * int(width)
    screen = bytes(capture).rsplit(b"\x1b[?1049h", 1)[-1].decode(errors="replace")
    row, column = 1, 0
    for text in re.split(r"(\x1b\[[0-?]*[ -/]*[@-~])", screen):
        if text.startswith("\x1b["):
            if match := re.fullmatch(r"\x1b\[(\d+);(\d+)H", text):
                row, column = map(int, match.groups())
                column -= 1
            elif text == "\x1b[2J":
                cells = [" "] * int(width)
            continue
        for char in text:
            if char == "\r":
                column = 0
            elif char == "\n":
                row += 1
            elif char.isprintable():
                size = 2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
                if row == number and 0 <= column < len(cells):
                    cells[column:column + size] = [char] + [""] * (size - 1)
                column += size
    return cells


def expect_screen_row(number, expected, *, exact=True):
    def read_row():
        return "".join(screen_cells(number)).rstrip()

    def matches(row):
        return row == expected if exact else expected in row

    limit = time.monotonic() + 12
    row = read_row()
    while not matches(row) and time.monotonic() < limit:
        if select.select([reader], [], [], 0.1)[0]:
            receive()
            row = read_row()
        elif process.poll() is not None:
            break
    assert matches(row), (expected, row)

def wait_for_screen(predicate):
    deadline = time.monotonic() + 12
    while True:
        result = predicate()
        if result:
            return result
        assert time.monotonic() < deadline, capture[-2000:]
        if select.select([reader], [], [], 0.1)[0]:
            receive()


def mouse_key(button, column, row, end=b"M"):
    return f"\x1b[<{button};{column + 1};{row}".encode() + end


try:
    if scenario == "portfolio-source-skip":
        expect("确认更新市场并升级")
        expect("将执行的原命令")
        expect("plugin marketplace add")
        expect("plugin update")
        expect("● 跳过此配置")
        type_key(b"\r")
    elif scenario in ("skill-agent-picker-cancel", "skill-agent-picker-all-cancel"):
        expect("全部 Agent")
        expect("codex")
        expect("cursor-agent")
        expect("选择 Agent")
        expected = [s.encode() for s in ("全部 Agent", "codex", "cursor-agent")]
        assert capture.index(expected[0]) < min(capture.index(label) for label in expected[1:])
        choose_all = scenario == "skill-agent-picker-all-cancel"
        offset = type_key(b"\x1b[A \r" if choose_all else b" \r")
        expect("共享目录会影响", offset)
        expect("确认执行", offset)
        plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(capture[offset:]))
        headers = ("Agent", "状态", "当前版本", "目标版本", "安装位置")
        assert any(all(h.encode() in line for h in headers) for line in plain.splitlines())
        rows = [rf"{label}[ ]+将执行" for label in ("codex", "cursor-agent")]
        count = sum(re.search(row.encode(), plain) is not None for row in rows)
        assert count == (2 if choose_all else 1)
        offset = type_key(b"\r")
        expect("已取消", offset)
    elif scenario in ("skill-adopt-style", "skill-adopt-no-color"):
        expect("已完成")
        expect("╭─")
        expect("确认操作")
        expect("╰─")
        expect("● 取消")
        expect("○ 确认执行")
        plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(capture)).decode(errors="replace")
        progress = [line for line in plain.splitlines() if line.startswith("│ ○ ")]
        assert any(line.startswith(("│ ○ ·  ", "│ ○ •  ")) for line in progress), progress
        assert any(line.endswith("  已完成") for line in progress), progress
        assert re.search(rb"\x1b\[\?25l\r\x1b\[2K[^\x0d]*?\x1b\[\?25h", capture)
        if scenario == "skill-adopt-style":
            expect("\x1b[1;36m●")
            expect("\x1b[1;36m○")
            expect("\x1b[1;32m○")
        else:
            assert not re.search(rb"\x1b\[[0-9;]*m", capture)
        offset = type_key(b"\x1b[B")
        expect("● 确认执行", offset)
        offset = type_key(b"\x1b[A")
        expect("● 取消", offset)
        offset = type_key(b"\r")
        expect("已取消", offset)
    elif scenario == "native-progress-mise":
        expect("fixture mise first frame")
        expect("fixture mise second frame")
        assert b"\x1b[1A" in capture and b"\x1b[2K" in capture
        assert b"\x1b]52;" not in capture and b"\x1b[?25l" not in capture
        assert process.poll() is None
        Path(target).touch()
        expect("已完成")
    elif scenario == "native-progress":
        expect("原生命令")
        expect("原工具输出 · 本机安装器")
        expect("fixture stdout progress")
        expect("fixture stderr progress")
        assert process.poll() is None
        Path(target).with_name("continue-progress").touch()
        expect("中\\u{1b}\\u{7} split text")
        Path(target).touch()
        expect("已完成")
    elif scenario == "native-progress-json":
        ready = Path(target).with_name("ready-progress")
        deadline = time.monotonic() + 8
        while not ready.exists() and time.monotonic() < deadline:
            if select.select([reader], [], [], 0.1)[0]:
                receive()
        assert ready.exists()
        Path(target).with_name("continue-progress").touch()
        Path(target).touch()
        expect('"phase": "succeeded"')
        assert b"fixture stdout progress" not in capture
        assert b"fixture stderr progress" not in capture
        assert "原生命令".encode() not in capture
        assert "原工具输出".encode() not in capture
    elif scenario == "migration-blocked":
        expect("migration_recovery_required")
        assert b"\x1b[?1049h" not in capture
    elif scenario == "setup-cancel":
        expect("推荐模型")
        expect("自定义 OpenAI 兼容服务")
        expect("当前模式 本地分析（尚未配置远端）")
        expect("选择")
        assert "测试已保存密钥".encode() not in capture
        offset = type_key(b"\r")
        expect("已取消", offset)
    elif scenario == "setup-hide-key":
        expect("推荐模型")
        type_key(b"2\r")
        expect("HTTPS Base URL")
        type_key(b"https://models.example.test/v1\r")
        expect("模型名")
        type_key(b"fixture-model\r")
        expect("API key")
        offset = type_key(b"fixture-model-keyX\x7f\r")
        expect("发送最小测试请求", offset)
        assert b"fixture-model-key" not in capture[offset:]
        offset = type_key(b"n\r")
        expect("已取消", offset)
    elif scenario in ("setup-key-escape", "setup-key-interrupt", "setup-new-isolated"):
        expect("推荐模型")
        type_key(b"1\r")
        expect("API key")
        if scenario == "setup-new-isolated":
            offset = type_key(b"fixture-model-key\r")
            expect("发送最小测试请求", offset)
            assert b"fixture-model-key" not in capture[offset:]
            offset = type_key(b"y\r")
            expect("model_keychain_unavailable", offset)
        else:
            end = b"\x1b" if scenario == "setup-key-escape" else b"\x03"
            offset = type_key(b"fixture-model-key" + end)
            expect("已取消", offset)
            assert b"fixture-model-key" not in capture[offset:]
    elif scenario == "setup-pause-saved":
        expect("当前模式")
        offset = type_key(b"3\r")
        expect("local", offset)
    elif scenario == "setup-write-busy":
        expect("当前模式")
        descriptor = os.open(target, os.O_RDONLY | os.O_DIRECTORY)
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            offset = type_key(b"3\r")
            expect("model_setup_busy", offset)
        finally:
            os.close(descriptor)
    elif scenario == "setup-recover-clean":
        expect("核对待完成的钥匙串操作")
        offset = type_key(b"c\r")
        expect("local", offset)
    elif scenario == "setup-saved-isolated":
        expect("测试已保存密钥")
        expect("更改模型")
        type_key(b"t\r")
        expect("发送最小测试请求")
        offset = type_key(b"y\r")
        expect("model_keychain_unavailable", offset)
    elif scenario == "setup-external-isolated":
        expect("绑定已有钥匙串条目")
        type_key(b"b\r")
        expect("HTTPS Base URL")
        type_key(b"https://models.example.test/v1\r")
        expect("模型名")
        type_key(b"fixture\r")
        expect("已有钥匙串文件的绝对路径")
        type_key(target.encode() + b"\r")
        expect("钥匙串服务名")
        type_key(b"fixture-service\r")
        expect("钥匙串账户名")
        type_key(b"fixture-account\r")
        expect("发送最小测试请求")
        offset = type_key(b"y\r")
        expect("model_keychain_unavailable", offset)
    elif scenario == "setup-single-invalid-model":
        expect("推荐模型")
        offset = type_key(b"m\r")
        expect("新的模型名", offset)
        offset = type_key(b"invalid model name\r")
        expect("model_setup_invalid", offset)
    elif scenario in ("setup-single-pending-edit", "setup-single-pending-confirm", "setup-single-pending-retain"):
        expect("推荐模型")
        answer, reason = {
            "setup-single-pending-edit": (b"2\r", "model_keychain_update_pending"),
            "setup-single-pending-confirm": (b"c\r", "model_keychain_update_pending"),
            "setup-single-pending-retain": (b"k\r", "model_keychain_cleanup_not_committed"),
        }[scenario]
        offset = type_key(answer)
        expect(reason, offset)
        assert b"API key" not in capture[offset:]
    elif scenario == "recommend-advice-only":
        expect("当前没有可执行的已核验安装计划")
    elif scenario == "recommend-local-maintenance":
        expect("本机状态")
        expect("安装方案")
        expect("维护入口")
        expect("选择维护入口")
        first, second, third = (capture.index(title.encode()) for title in ("本机状态", "安装方案", "维护入口"))
        assert first < second < third
        offset = type_key(b"\x1b[B\r")
        expect("\x1b[?1049h", offset)
        expect("fixture-alpha", offset)
        type_key(b"r")
        offset = type_key(b"\r")
        expect("安装状态", offset)
        assert b"fixture-beta" not in capture
        type_key(b"q")
    elif scenario == "recommend-picker-cancel":
        expect("选择安装候选")
        offset = type_key(b"99\r")
        expect("请输入列表中的编号", offset)
        type_key(b"\r")
    elif scenario in ("recommend-install", "recommend-cancel", "recommend-mise", "recommend-install-unknown"):
        expect("选择安装候选")
        if scenario == "recommend-install":
            for invalid in (b"0\r", b"99\r", b"typo\r"):
                offset = type_key(invalid)
                expect("请输入列表中的编号", offset)
            assert "输入精确版本".encode() not in capture
        type_key(b"1\r")
        expect("输入精确版本")
        type_key(b"1.2.3\r")
        expect("安装并纳入管理")
        expect("用户级配置" if scenario == "recommend-mise" else "安装范围")
        expect("确认执行")
        assert "输入 yes".encode() not in capture
        offset = type_key(b"\r" if scenario == "recommend-cancel" else b"\x1b[B\r")
        outcome = {
            "recommend-cancel": "已取消",
            "recommend-install-unknown": "结果 还没确认",
        }.get(scenario, "已完成")
        expect(outcome, offset)
        if scenario == "recommend-install-unknown":
            state = Path(command[command.index("--state-dir") + 1])
            operation = json.loads((state / "active.json").read_bytes())["id"]
            expect("运行 mo recover 可查看记录", offset)
            assert operation.encode() not in capture
    elif scenario == "adopt-picker-confirmation-cancel":
        expect("Enter 选择")
        expect("picker-left")
        assert b"picker-right" not in capture
        offset = type_key(b"\r")
        expect("确认执行", offset)
        expect(str(target), offset)
        offset = type_key(b"no\r")
        expect("已取消", offset)
    elif scenario in ("adopt-picker-cancel", "adopt-picker-select"):
        expect("Enter 选择")
        expect("渠道")
        expect("安装范围")
        expect("npm")
        expect("picker-left")
        expect("picker-right")
        assert b"picker-missing" not in capture
        offset = type_key(b"/")
        expect("搜索", offset)
        type_key(Path(target).name.encode() + b"\r")
        if scenario == "adopt-picker-cancel":
            offset = type_key(b"\x1b")
            expect("已取消", offset)
        else:
            offset = type_key(b"\r")
            expect("确认执行", offset)
            expect(str(target), offset)
            offset = type_key(b"\x1b[B\r")
            expect("已完成", offset)
    elif scenario == "market-json":
        expect('"kind": "plugin-marketplaces"')
        limit = time.monotonic() + 8
        while time.monotonic() < limit:
            if select.select([reader], [], [], 0.1)[0]:
                before = len(capture)
                receive()
                if len(capture) == before:
                    break
            elif process.poll() is not None:
                break
        assert json.loads(capture)["data"]["kind"] == "plugin-marketplaces"
    elif scenario in ("market-browser", "market-entry", "market-online", "market-partial"):
        if scenario == "market-online":
            expect("正在查询 Cursor 账户市场")
        expect("插件市场")
        expect("Enter 详情")
        if scenario == "market-browser":
            expect("fixture-codex")
            type_key(b"/")
            expect_screen_row(2, "/ 搜索 · ▏")
            type_key(b"two\r")
            member = "two"
        elif scenario == "market-entry":
            expect("cursor:default")
            member = "mo skill market cursor --online"
        elif scenario == "market-partial":
            expect("部分市场无法核验")
            member = "市场状态未知"
        else:
            expect("team")
            member = "https://github.com/vendor/cursor"
            type_key(b"r")
        offset = type_key(b"\r")
        expect(member, offset)
        if scenario == "market-browser":
            offset = type_key(b"\x1b")
            expect("Enter 详情", offset)
        type_key(b"q")
    elif scenario == "inventory-reject-scope":
        expect("Tab 切换")
        type_key(b"/" + target.encode() + b"\r")
        offset = type_key(b"\r")
        expect("a 检查管理资格", offset)
        offset = type_key(b"a")
        expect("config_scope_unsupported", offset)
        type_key(b"q")
    elif scenario in ("detail-app-readonly", "detail-declared-upgrade", "detail-version-upgrade", "detail-declared-install", "detail-app-uninstall"):
        expect("软件")
        offset = type_key(b"/" + target.encode() + b"\r")
        expect(target, offset)
        offset = type_key(b"\r")
        if scenario == "detail-app-readonly":
            expect("仅盘点", offset)
            offset = type_key(b"u")
            expect("暂无已核验的维护入口", offset)
            type_key(b"a")
            expect_screen_row(27, "这份安装暂无可登记的维护入口。请查看详情中的渠道和安装范围。")
        elif scenario == "detail-app-uninstall":
            expect("x 卸载", offset)
            offset = type_key(b"x")
            expect("Enter 确认卸载", offset)
            offset = type_key(b"\r")
            expect("卸载结果 已完成", offset)
        elif scenario == "detail-declared-install":
            expect("i 安装", offset)
            offset = type_key(b"i")
            expect("目标版本 2.0.0", offset)
            expect("Enter 确认安装", offset)
            offset = type_key(b"\r")
            expect("安装结果 已完成", offset)
        else:
            expect("u 升级", offset)
            offset = type_key(b"u")
            if scenario == "detail-version-upgrade":
                expect("输入 demo 的目标版本", offset)
                offset = type_key(b"\r")
                expect("已取消。", offset)
                offset = type_key(b"u")
                expect("输入 demo 的目标版本", offset)
                offset = type_key(b"2.4.0\r")
                expect("目标版本 2.4.0", offset)
            expect("Enter 确认升级", offset)
            offset = type_key(b"\r")
            expect("升级结果 已完成", offset)
        type_key(b"q")
    elif scenario == "detail-discovered-skill-uninstall":
        expect("技能清单")
        type_key(b"/" + target.encode() + b"\r")
        offset = type_key(b"\r")
        expect("x 卸载", offset)
        offset = type_key(b"x")
        expect("卸载 " + target + " · Skills CLI", offset)
        expect("共享安装", offset)
        expect("确认执行", offset)
        offset = type_key(b"\x1b[B\r")
        expect("原生命令", offset)
        expect("remove --global --yes --agent codex --skill alpha", offset)
        expect("原工具输出 · Skills CLI", offset)
        expect("fixture Skills CLI removed alpha", offset)
        expect("卸载结果 已完成", offset)
        type_key(b"q")
    elif scenario == "detail-discovered-skill-adopt":
        expect("技能清单")
        type_key(b"/" + target.encode() + b"\r")
        offset = type_key(b"\r")
        expect("a 接管技能", offset)
        offset = type_key(b"a")
        expect("接管现有技能", offset)
        expect("确认执行", offset)
        offset = type_key(b"\x1b[B\r")
        expect("技能管理结果", offset)
        type_key(b"q")
    elif scenario in ("detail-config-operations", "detail-config-apply"):
        expect("受管配置清单")
        offset = type_key(b"/.demo\r")
        expect(".demo", offset)
        offset = type_key(b"\r")
        expect("p 应用", offset)
        offset = type_key(b"d")
        expect("配置差异", offset)
        offset = type_key(b"\x1b")
        expect("v 核验", offset)
        offset = type_key(b"v")
        expect("配置核验结果", offset)
        offset = type_key(b"\x1b")
        expect("p 应用", offset)
        offset = type_key(b"p")
        expect("Native apply", offset)
        if scenario == "detail-config-apply":
            offset = type_key(b"\x1b[B\r")
            expect("配置应用结果", offset)
        else:
            offset = type_key(b"\r")
            expect("已取消", offset)
        type_key(b"q")
    elif scenario == "detail-agent-upgrade":
        expect("Agent")
        offset = type_key(b"r")
        expect("1.0.0", offset)
        offset = type_key("/本机固定版本 claude\r".encode())
        expect("claude", offset)
        offset = type_key(b"\r")
        expect("u 升级 Agent CLI", offset)
        offset = type_key(b"u")
        expect("升级 Agent CLI claude", offset)
        offset = type_key(b"\r")
        expect("已取消", offset)
        type_key(b"q")
    elif scenario in ("detail-agent-version-cancel", "detail-agent-version-apply"):
        expect("Agent")
        type_key(b"r")
        offset = type_key("/本机固定版本 dsh\r".encode())
        expect("dsh", offset)
        offset = type_key(b"\r")
        expect("u 升级 Agent CLI", offset)
        offset = type_key(b"u")
        expect("输入 dsh 的新版本", offset)
        offset = type_key(b"2.0.0\r")
        expect("将 dsh 升级到 2.0.0", offset)
        offset = type_key(b"\x1b[B\r" if scenario == "detail-agent-version-apply" else b"\r")
        expect("已完成" if scenario == "detail-agent-version-apply" else "已取消", offset)
        type_key(b"q")
    elif scenario in ("detail-agent-forget", "detail-agent-forget-cancel", "detail-agent-forget-stale"):
        expect("Agent")
        type_key(b"/" + target.encode() + b"\r")
        offset = type_key(b"\r")
        expect("x 停止接管", offset)
        offset = type_key(b"x")
        expect("停止接管", offset)
        expect("保留本机程序、配置和插件", offset)
        expect("确认执行", offset)
        if scenario == "detail-agent-forget-stale":
            home = Path(command[command.index("--home") + 1])
            config = home / "config-one"
            config.rename(home / "config-one-before")
            config.mkdir()
        offset = type_key(b"\r" if scenario == "detail-agent-forget-cancel" else b"\x1b[B\r")
        expect({"detail-agent-forget": "已停止接管", "detail-agent-forget-cancel": "已取消", "detail-agent-forget-stale": "档案目录已变化"}[scenario], offset)
        type_key(b"q")
    elif scenario in ("detail-codex-remove", "detail-codex-fixed-refusal"):
        expect("软件")
        offset = type_key(b"\t")
        expect("widget@fixture", offset)
        offset = type_key(b"/widget\r")
        expect("widget", offset)
        offset = type_key(b"\r")
        expect("卸载插件", offset)
        if scenario == "detail-codex-fixed-refusal":
            expect("按固定配方检查并重装", offset)
            offset = type_key(b"u")
            expect("固定原生插件配方的本地证据不足", offset)
        else:
            offset = type_key(b"x")
            expect("暂不能操作", offset)
        type_key(b"q")
    elif scenario == "detail-plugin-scope":
        expect("软件")
        offset = type_key(b"\t")
        expect("能力", offset)
        type_key(b"/widget @claude:mi\r")
        expect_screen_row(4, "/ 搜索 · widget @claude:mi")
        offset = type_key(b"\r")
        expect("e 启用", offset)
        offset = type_key(b"e")
        expect("已达到目标", offset)
        type_key(b"q")
    elif scenario in ("inventory-adopt", "inventory-adopt-cancel", "inventory-remove", "inventory-upgrade-cancel", "inventory-upgrade-migration"):
        expect("Tab 切换")
        offset = type_key(b"/" + target.encode() + b"\r")
        expect(target, offset)
        offset = type_key(b"\r")
        expect("a 检查管理资格", offset)
        trigger, action = {
            "inventory-remove": (b"x", "卸载"),
            "inventory-upgrade-cancel": (b"u", "升级"),
            "inventory-upgrade-migration": (b"u", "升级"),
        }.get(scenario, (b"a", "纳入管理"))
        offset = type_key(trigger)
        expect("Enter 确认" + action, offset)
        expect("当前版本 1.2.3", offset)
        if scenario in ("inventory-adopt", "inventory-remove"):
            offset = type_key(b"\r")
            expect(action + "结果 已完成", offset)
        elif scenario == "inventory-upgrade-migration":
            state = Path(command[command.index("--state-dir") + 1])
            (state / "migration-active.json").write_text("interrupted migration")
            offset = type_key(b"\r")
            expect("migration_recovery_required", offset)
        else:
            offset = type_key(b"\x1b")
            expect("a 检查管理资格", offset)
        type_key(b"q")
    elif scenario in ("mise-adopt", "mise-upgrade-cancel", "mise-upgrade-conflict", "mise-upgrade", "mise-retire"):
        expect("软件")
        offset = type_key(b"/" + target.encode() + b"\r")
        expect(target, offset)
        offset = type_key(b"\r")
        expect("a 检查管理资格", offset)
        if scenario == "mise-retire":
            offset = type_key(b"x")
            expect("consumer_review_required", offset)
        else:
            adopt = scenario == "mise-adopt"
            action = "纳入管理" if adopt else "升级"
            offset = type_key(b"a" if adopt else b"u")
            expect("Enter 确认" + action, offset)
            expect("当前版本 1.2.3", offset)
            expect("安装范围", offset)
            if not adopt:
                expect("目标版本 2.0.0", offset)
                expect("安装并更新用户级配置中的版本", offset)
            if scenario == "mise-upgrade-cancel":
                offset = type_key(b"\x1b")
                expect("a 检查管理资格", offset)
            elif scenario == "mise-upgrade-conflict":
                catalog = Path(command[command.index("--catalog") + 1])
                data = json.loads(catalog.read_text())
                duplicate = dict(data["packages"][0], id="fixture:mise-duplicate")
                data["packages"].append(duplicate)
                catalog.write_text(json.dumps(data))
                offset = type_key(b"\r")
                expect("plan_stale", offset)
            else:
                offset = type_key(b"\r")
                expect(action + "结果 已完成", offset)
        type_key(b"q")
    elif scenario in ("mise-retirement-cancel", "mise-retirement-stale", "mise-retirement-confirm"):
        expect("软件")
        offset = type_key(b"/" + target.encode() + b"\r")
        expect(target, offset)
        offset = type_key(b"\r")
        expect("x 卸载", offset)
        offset = type_key(b"x")
        expect("Enter 确认卸载", offset)
        expect("移除版本 1.2.3", offset)
        expect("其他项目仍可能使用此版本", offset)
        if scenario == "mise-retirement-cancel":
            offset = type_key(b"\x1b")
            expect("x 卸载", offset)
        elif scenario == "mise-retirement-stale":
            home = Path(command[command.index("--home") + 1])
            payload = home / ".local/share/mise/installs/demo/1.2.3"
            payload.rename(home / "original-retirement-payload")
            payload.mkdir()
            (payload / "tool").write_text("same observable version")
            offset = type_key(b"\r")
            expect("载荷或原生程序已改变", offset)
        else:
            offset = type_key(b"\r")
            expect("卸载结果 已完成", offset)
        type_key(b"q")
    elif scenario == "inventory-table-controls":
        expect("Shift+←/→ 横移")
        header = wait_for_screen(lambda: next((
            row for row in range(3, 10)
            if "管理状态" in "".join(screen_cells(row))
        ), None))
        chosen = "".join(screen_cells(header + 1)).split()[1]
        type_key(b"\x1b[1;2C")
        expect_screen_row(header, "安装范围", exact=False)
        expect_screen_row(header, "软件", exact=False)
        expect_screen_row(header + 1, f"> {chosen}", exact=False)
        assert "管理状态" not in "".join(screen_cells(header))
        offset = type_key(b"\r")
        expect("原生标识 fixture:demo", offset)
        type_key(b"\x1b")
        expect_screen_row(header, "安装范围", exact=False)
        type_key(b"\t")
        expect_screen_row(1, "[能力]", exact=False)
        type_key(b"\x1b[Z")
        expect_screen_row(1, "[软件]", exact=False)
        expect_screen_row(header, "安装范围", exact=False)
        assert "管理状态" not in "".join(screen_cells(header))
        type_key(b"\x1b[1;2D")
        expect_screen_row(header, "管理状态", exact=False)

        offset = type_key(b"w")
        expect("\x1b[?1006h", offset)
        expect("拖表头")
        divider = wait_for_screen(lambda: next((
            x for x, cell in enumerate(screen_cells(header)[1:-1], 1) if cell == "│"
        ), None))
        type_key(mouse_key(35, divider, header) +
                 mouse_key(0, divider, header) +
                 mouse_key(32, divider + 4, header) +
                 mouse_key(0, divider + 4, header, b"m"))
        wait_for_screen(lambda: screen_cells(header)[divider + 4] == "│")
        type_key(b"\x1b[A")
        wait_for_screen(lambda: screen_cells(header)[divider + 5] == "│")
        type_key(mouse_key(0, divider + 5, header) +
                 mouse_key(32, divider + 7, header))
        wait_for_screen(lambda: screen_cells(header)[divider + 7] == "│")
        fcntl.ioctl(writer, termios.TIOCSWINSZ, struct.pack("HHHH", height + 1, int(width), 0, 0))
        process.send_signal(signal.SIGWINCH)
        wait_for_screen(lambda: screen_cells(header)[divider + 5] == "│")
        type_key(mouse_key(0, divider + 5, header) +
                 mouse_key(32, divider + 7, header))
        wait_for_screen(lambda: screen_cells(header)[divider + 7] == "│")
        offset = type_key(b"\x1b")
        expect("\x1b[?1006l", offset)
        type_key(b"\t")
        expect_screen_row(1, "[能力]", exact=False)
        type_key(b"\x1b[Z")
        expect_screen_row(1, "[软件]", exact=False)
        type_key(b"w")
        wait_for_screen(lambda: screen_cells(header)[divider + 5] == "│")
        type_key(b"0")
        wait_for_screen(lambda: screen_cells(header)[divider] == "│")
        offset = type_key(b"q")
        expect("\x1b[?1006l", offset)
    elif scenario == "app-version-evidence":
        expect("版本")
        expect("3.1.4")
        assert not (Path(target) / "version-probe").exists()
        offset = type_key(b"r")
        expect("8.7.6", offset)
        type_key(b"q")
    elif scenario == "unified-list":
        expect("Tab 切换")
        offset = type_key(b"\t")
        expect("fixture-loose", offset)
        expect("全部发现", offset)
        offset = type_key(b"\r")
        expect("载荷", offset)
        offset = type_key(b"\x1b")
        expect("Enter 详情", offset)
        offset = type_key(b"a")
        expect("已登记筛选", offset)
        assert b"fixture-loose" not in capture[offset:]
        type_key(b"q")
    elif scenario == "skills-discovered":
        expect("管理状态")
        expect("安装位置")
        expect("fixture-skill")
        expect("fixture-loose")
        expect("全部发现")
        offset = type_key(b"a")
        expect("已登记筛选", offset)
        type_key(b"/fixture-loose\r")
        offset = type_key(b"r")
        expect("0 / 2", offset)
        offset = type_key(b"a")
        expect("fixture-loose", offset)
        offset = type_key(b"\r")
        expect("载荷", offset)
        type_key(b"q")
    elif scenario in ("recommend-already-blocked", "recommend-blocked-after-menu"):
        expect("安装建议 · demo")
        record = Path(target)
        expect("选择安装候选")
        if scenario == "recommend-blocked-after-menu":
            state = Path(command[command.index("--state-dir") + 1])
            (state / "active.json").write_bytes(record.read_bytes())
        type_key(b"1\r")
        operation = json.loads(record.read_bytes())["id"]
        expect("1 demo")
        expect("输入精确版本")
        type_key(b"1.0.0\r")
        expect("确认后只核对那次操作")
        assert "执行此安装计划".encode() not in capture
        type_key(b"\x1b[B\r")
        expect("还没开始新操作")
        expect("运行 mo recover 查看待检查的操作")
        text = capture.decode(errors="replace")
        assert "执行此安装计划" not in text and operation not in text
    elif scenario in ("recommend-cancel-version", "recommend-retry-version", "recommend-other-version"):
        expect("选择安装候选")
        type_key(b"1\r")
        expect("输入精确版本")
        if scenario == "recommend-cancel-version":
            type_key(b"\r")
        elif scenario == "recommend-retry-version":
            operation = json.loads(Path(target).read_bytes())["id"]
            type_key(b"1.2.3\r")
            expect("结果 已完成")
            assert operation.encode() not in capture
        else:
            type_key(b"2.0.0\r")
            expect("确认后只核对那次操作")
            assert "执行此安装计划".encode() not in capture
            type_key(b"\x1b[B\r")
            expect("already_installed")
            assert "结果 已完成".encode() not in capture
    elif scenario in ("recommend-skill-cancel", "recommend-skill-stale"):
        expect("Skills CLI · Codex 用户级 · 共享技能目录")
        type_key(b"1\r")
        expect("仅手动触发 是")
        expect("其他支持的 Agent 也可能发现")
        if scenario == "recommend-skill-stale":
            import json
            book = Path(target)
            data = json.loads(book.read_text())
            data["recipes"][0]["source"]["commit"] = "b" * 40
            book.write_text(json.dumps(data))
            offset = type_key(b"\x1b[B\r")
            expect("plan_stale", offset)
        else:
            type_key(b"n\r")
    elif scenario == "history-empty":
        expect("暂无操作记录")
        type_key(b"q")
    elif scenario in ("history-direct", "history-unified"):
        if scenario == "history-unified":
            expect("Tab 切换")
            offset = type_key(b"\x1b[Z")
            expect("按开始时间排序", offset)
        else:
            expect("按开始时间排序")
        offset = type_key(b"\r")
        expect("mo recover OP-fixture-recent --dry-run", offset)
        expect("核对原结果", offset)
        expect("保留当前运行环境及作用域参数", offset)
        assert b"SYNTHETIC_PRIVATE_COMMAND_OUTPUT" not in capture
        type_key(b"q")
    elif scenario == "history-active-unreadable":
        expect("活动操作记录无法读取，仅显示历史")
        offset = type_key(b"\r")
        expect("OP-fixture-unreadable-active", offset)
        assert b"mo recover" not in capture
        assert "c 核对原操作".encode() not in capture
        type_key(b"q")
    elif scenario in ("history-recover-cancel", "history-recover-stale", "history-recover-migration", "history-recover-confirm"):
        expect("按开始时间排序")
        offset = type_key(b"\r")
        expect("c 核对原操作", offset)
        offset = type_key(b"c")
        expect("Enter 确认核对", offset)
        expect("OP-fixture-recovery", offset)
        expect("回退该操作的改动", offset)
        if scenario == "history-recover-cancel":
            offset = type_key(b"\x1b")
            expect("c 核对原操作", offset)
        else:
            if scenario == "history-recover-stale":
                import json
                active = Path(target)
                changed = json.loads(active.read_text())
                changed["message"] = "operation progress changed during confirmation"
                active.write_text(json.dumps(changed))
            if scenario == "history-recover-migration":
                Path(target).with_name("migration-active.json").write_text("interrupted migration")
            offset = type_key(b"\r")
            outcome = {
                "history-recover-stale": "plan_stale",
                "history-recover-migration": "migration_recovery_required",
                "history-recover-confirm": "核对结果 已记录失败",
            }[scenario]
            expect(outcome, offset)
        type_key(b"q")
    elif scenario == "skill":
        expect("技能清单")
        expect("fixture-skill")
        type_key(b"/@codex\r")
        expect_screen_row(3, "/ 搜索 · @codex")
        offset = type_key(b"\r")
        expect("仅手动触发", offset)
        type_key(b"\x03")
    elif scenario == "skill-visibility":
        expect("技能清单")
        type_key(b"/fixture-skill @cursor\r")
        expect("fixture-skill")
        offset = type_key(b"\r")
        expect("安装状态 在磁盘上", offset)
        expect("可发现 cursor:default · 共享目录 · " + target + "/.agents/skills/fixture-skill", offset)
        expect("可发现 claude:mi · 档案目录 · " + target + "/.claude-mi/skills/fixture-skill", offset)
        expect("载荷 在磁盘上 · " + target + "/.agents/skills/fixture-skill", offset)
        expect("会话载入 未知", offset)
        expect("可发现 codex:default", offset)
        type_key(b"q")
    elif scenario in ("skill-claude-import", "skill-codex-import"):
        name = scenario.removeprefix("skill-")
        expect("全部发现")
        expect(name)
        type_key(f"/{name}\r".encode())
        offset = type_key(b"\r")
        expect("可发现 cursor:default", offset)
        if name == "claude-import":
            expect("可发现 claude:default", offset)
        type_key(b"q")
    elif scenario == "skill-location-groups":
        narrow = int(width) < 80
        list_row = 7 if narrow else 6
        search_row = 4 if narrow else 3
        labels = ("Claude Code", "claude:glm", "claude:mi", "Codex", "Cursor", "共享技能")
        second_row = ("Cursor", "共享技能") if narrow else ()
        expect_screen_row(2, "←/→ 安装位置 · [全部]", exact=False)
        for label in labels:
            row = 3 if label in second_row else 2
            expect_screen_row(row, label, exact=False)
        for label in labels:
            offset = type_key(b"\x1b[C")
            expect_screen_row(3 if label in second_row else 2, f"[{label}]", exact=False)
            if label == "Cursor":
                expect_screen_row(list_row, "cursor-only", exact=False)
                assert all(name not in capture[offset:] for name in (b"claude-import", b"codex-import", b"fixture-skill"))
            elif label == "claude:glm":
                expect_screen_row(list_row, "glm-only", exact=False)
            elif label in ("claude:mi", "共享技能"):
                expect_screen_row(list_row, "fixture-skil" if narrow else "fixture-skill", exact=False)
        type_key("/一个很长的搜索词 @claude:mi\r".encode())
        expect_screen_row(search_row, "/ 搜索 · 一个很长的搜索词 @claude:mi")
        expect_screen_row(3 if narrow else 2, "[共享技能]", exact=False)
        type_key(b"q")
    elif scenario == "skill-short-location-groups":
        expect_screen_row(2, "[全部]", exact=False)
        for _ in range(6):
            type_key(b"\x1b[C")
        expect_screen_row(3, "[共享技能]", exact=False)
        expect_screen_row(4, "/ 搜索")
        expect_screen_row(7, "│> fixtu", exact=False)
        type_key(b"q")
    elif scenario == "skill-partial":
        expect("能力盘点不完整")
        offset = type_key(b"/fixture-skill\r\r")
        expect("可发现 codex:default", offset)
        expect("可发现 cursor:default", offset)
        type_key(b"q")
    elif scenario in ("skill-maintenance-cancel", "skill-maintenance-stale", "skill-maintenance-removed"):
        expect("fixture-skill")
        offset = type_key(b"/fixture-skill\r\r")
        expect("u 升级", offset)
        offset = type_key(b"u" if scenario == "skill-maintenance-cancel" else b"x")
        expect("Codex 用户级共享技能目录", offset)
        expect("上游成员 fixture-skill", offset)
        if scenario != "skill-maintenance-cancel":
            expect("将无法再发现该技能", offset)
        expect("已登记提交", offset)
        expect("确认执行", offset)
        if scenario == "skill-maintenance-stale":
            import json
            book = Path(target)
            changed = json.loads(book.read_text())
            changed["recipes"][0]["source"]["commit"] = "b" * 40
            book.write_text(json.dumps(changed))
        offset = type_key(b"\r" if scenario == "skill-maintenance-cancel" else b"\x1b[B\r")
        result = {
            "skill-maintenance-cancel": "已取消",
            "skill-maintenance-stale": "plan_stale",
            "skill-maintenance-removed": "卸载结果 已完成",
        }[scenario]
        expect(result, offset)
        type_key(b"q")
    elif scenario == "plugin-visibility":
        expect("Tab 切换")
        offset = type_key(b"\t")
        expect("本机能力清单", offset)
        profile, enabled = target.split(" ", 1)
        offset = type_key(b"/@" + profile.encode() + b"\r\r")
        expect(f"插件配置 {profile} · {enabled}", offset)
        expect("安装记录", offset)
        expect("会话载入 未知", offset)
        type_key(b"q")
    elif scenario in ("refresh", "refresh-retry"):
        expect("尚未确认受管配置清单" if scenario == "refresh-retry" else "受管配置清单")
        offset = type_key(b"r")
        expect("差异状态待查", offset)
        if scenario == "refresh-retry":
            Path(target).unlink()
            offset = type_key(b"r")
            expect("受管配置清单", offset)
        type_key(b"q")
    elif scenario == "agent-refresh":
        expect("Agent")
        offset = type_key(b"r")
        expect("1.2.3", offset)
        expect("codex", offset)
        expect("Agent CLI 版本或档案证据不完整", offset)
        type_key(b"q")
    elif scenario == "agent-profile-version":
        expect("待核验")
        assert not (Path(target) / "version-probe").exists()
        type_key(b"r")
        expect_screen_row(6, "8.7.6", exact=False)
        expect_screen_row(7, "待核验", exact=False)
        type_key(b"q")
    elif scenario == "archived":
        expect("绑定已变化")
        offset = type_key(b"\r")
        expect("o 打开实际文件", offset)
        offset = type_key(b"o")
        expect("需要当前作用域内已核验的普通文件", offset)
        assert b"synthetic-live-content" not in capture
        type_key(b"q")
    elif scenario == "tool":
        expect("受管配置清单")
        program = Path(target)
        replacement = program.with_name("replacement-chezmoi")
        replacement.write_text('#!/bin/sh\nprintf started > "$HOME/replacement-executed"\nexit 98\n')
        replacement.chmod(0o700)
        replacement.replace(program)
        offset = type_key(b"r")
        expect("绑定已变化", offset)
        type_key(b"q")
    elif scenario == "directory":
        expect("受管配置清单")
        assert b".group-directory" not in capture
        offset = type_key(b"d")
        expect(".group-directory", offset)
        type_key(b"q")
    else:
        expect("受管配置清单")
        assert b"synthetic-live-content" not in capture
        offset = type_key(b"\r")
        expect("o 打开实际文件", offset)
        assert b"synthetic-live-content" not in capture
        if scenario == "replaced":
            replacement = Path(target).with_suffix(".new")
            replacement.write_text("foreign-replacement")
            replacement.replace(target)
        elif scenario == "root":
            root = Path(target).parent
            root.rename(root.with_name("retired-root"))
            root.mkdir()
            Path(target).write_text("foreign-replacement")
        elif scenario == "owner":
            state = Path(command[command.index("--state-dir") + 1])
            owner = state / "chezmoi-owner.json"
            renewal = state / "renewed-owner.json"
            renewal.write_bytes(owner.read_bytes())
            renewal.chmod(0o600)
            renewal.replace(owner)
        offset = type_key(b"o")
        if scenario == "replaced":
            expect("所选文件身份或内容已改变", offset)
            assert b"foreign-replacement" not in capture
        elif scenario == "link":
            expect("需要当前作用域内已核验的普通文件", offset)
            assert b"synthetic-live-content" not in capture
        elif scenario == "large":
            expect("所选文件超过查看限制", offset)
        elif scenario == "binary":
            expect("所选文件包含二进制数据", offset)
        elif scenario in ("root", "owner"):
            expect("原生工具或配置 owner 身份改变", offset)
            assert b"foreign-replacement" not in capture
            assert b"synthetic-live-content" not in capture
        elif scenario == "long":
            expect("xxxxxxxxxxxx", offset)
            assert b"tail-of-long-single-line" not in capture
            offset = type_key(b"\x04" * 8)
            expect("tail-of-long-single-line", offset)
        else:
            expect("synthetic-live-content", offset)
            assert b"\x1b]52;" not in capture
            offset = type_key(b"\x1b")
            expect("o 打开实际文件", offset)
            offset = type_key(b"s")
            expect("synthetic-source-content", offset)
        type_key(b"q")
    if scenario not in ("market-json", "migration-blocked", "portfolio-source-skip") and not scenario.startswith(("setup-", "recommend-", "native-progress", "skill-adopt-", "skill-agent-")):
        expect("\x1b[?1049l")
    deadline = time.monotonic() + 8
    while process.poll() is None and time.monotonic() < deadline:
        if select.select([reader], [], [], 0.1)[0]:
            receive()
    actual_exit = process.wait(timeout=max(deadline - time.monotonic(), 0.1))
    assert actual_exit == int(exit_code), (scenario, actual_exit, exit_code)
    assert termios.tcgetattr(writer) == original, "terminal settings were not restored"
finally:
    record = os.environ.get("MACOPS_BROWSER_TRANSCRIPT")
    if record:
        dest = Path(record)
        dest.mkdir(parents=True, exist_ok=True)
        (dest / f"{scenario}-{width}.ansi").write_bytes(capture)
    if process.poll() is None:
        process.kill()
        process.wait(timeout=5)
    os.close(writer)
    os.close(reader)
