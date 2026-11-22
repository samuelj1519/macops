"""Drive the compiled CLI on a disposable terminal using synthetic profiles."""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import sys
import termios
import time

scenario, target, width, exit_code, *command = sys.argv[1:]
reader, writer = pty.openpty()
fcntl.ioctl(writer, termios.TIOCSWINSZ, struct.pack("HHHH", 28, int(width), 0, 0))
original = termios.tcgetattr(writer)
capture = bytearray()


process = subprocess.Popen(
    command,
    stdin=writer,
    stdout=writer,
    stderr=writer,
    env=dict(os.environ, TERM="xterm-256color"),
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


try:
    if scenario == "setup-cancel":
        expect("推荐模型")
        expect("自定义 OpenAI 兼容服务")
        offset = type_key(b"\r")
        expect("cancelled", offset)
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
        expect("cancelled", offset)
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
            expect("cancelled", offset)
            assert b"fixture-model-key" not in capture[offset:]
    elif scenario == "setup-pause-saved":
        expect("当前模式")
        offset = type_key(b"3\r")
        expect("local", offset)
    elif scenario == "setup-recover-clean":
        expect("核对待完成的钥匙串操作")
        offset = type_key(b"c\r")
        expect("local", offset)
    elif scenario == "setup-saved-isolated":
        expect("测试已保存密钥")
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
    elif scenario == "recommend-advice-only":
        expect("当前没有可执行的已核验安装计划")
    elif scenario in ("recommend-install", "recommend-cancel", "recommend-mise"):
        expect("选择安装候选")
        type_key(b"1\r")
        expect("输入精确版本")
        type_key(b"1.2.3\r")
        expect("安装并纳管")
        expect("用户级配置" if scenario == "recommend-mise" else "安装范围")
        offset = type_key(b"no\r" if scenario == "recommend-cancel" else b"yes\r")
        expect("cancelled" if scenario == "recommend-cancel" else "succeeded", offset)
    elif scenario == "adopt-picker-confirmation-cancel":
        expect("Enter 选择")
        expect("picker-left")
        assert b"picker-right" not in capture
        offset = type_key(b"\r")
        expect("输入 yes", offset)
        expect(str(target), offset)
        offset = type_key(b"no\r")
        expect("cancelled", offset)
    elif scenario in ("adopt-picker-cancel", "adopt-picker-select"):
        expect("Enter 选择")
        expect("npm ·")
        expect("picker-left")
        expect("picker-right")
        assert b"picker-missing" not in capture
        offset = type_key(b"/")
        expect("搜索", offset)
        type_key(Path(target).name.encode() + b"\r")
        if scenario == "adopt-picker-cancel":
            offset = type_key(b"\x1b")
            expect("cancelled", offset)
        else:
            offset = type_key(b"\r")
            expect("输入 yes", offset)
            expect(str(target), offset)
            offset = type_key(b"yes\r")
            expect("succeeded", offset)
    elif scenario == "inventory-reject-scope":
        expect("Tab 切换")
        type_key(b"/" + target.encode() + b"\r")
        offset = type_key(b"\r")
        expect("a 检查纳管", offset)
        offset = type_key(b"a")
        expect("config_scope_unsupported", offset)
        type_key(b"q")
    elif scenario in ("inventory-adopt", "inventory-adopt-cancel", "inventory-remove", "inventory-upgrade-cancel"):
        expect("Tab 切换")
        offset = type_key(b"/" + target.encode() + b"\r")
        expect(target, offset)
        offset = type_key(b"\r")
        expect("a 检查纳管", offset)
        trigger, action = {
            "inventory-remove": (b"x", "卸载"),
            "inventory-upgrade-cancel": (b"u", "升级"),
        }.get(scenario, (b"a", "纳管"))
        offset = type_key(trigger)
        expect("Enter 确认" + action, offset)
        expect("当前版本 1.2.3", offset)
        if scenario in ("inventory-adopt", "inventory-remove"):
            offset = type_key(b"\r")
            expect(action + "结果 succeeded", offset)
        else:
            offset = type_key(b"\x1b")
            expect("a 检查纳管", offset)
        type_key(b"q")
    elif scenario in ("mise-adopt", "mise-upgrade-cancel", "mise-upgrade-conflict", "mise-upgrade", "mise-retire"):
        expect("软件")
        offset = type_key(b"/" + target.encode() + b"\r")
        expect(target, offset)
        offset = type_key(b"\r")
        expect("a 检查纳管", offset)
        if scenario == "mise-retire":
            offset = type_key(b"x")
            expect("consumer_review_required", offset)
        else:
            adopt = scenario == "mise-adopt"
            action = "纳管" if adopt else "升级"
            offset = type_key(b"a" if adopt else b"u")
            expect("Enter 确认" + action, offset)
            expect("当前版本 1.2.3", offset)
            expect("安装范围", offset)
            if not adopt:
                expect("目标版本 2.0.0", offset)
                expect("安装并更新用户级配置中的版本", offset)
            if scenario == "mise-upgrade-cancel":
                offset = type_key(b"\x1b")
                expect("a 检查纳管", offset)
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
                expect(action + "结果 succeeded", offset)
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
            expect("卸载结果 succeeded", offset)
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
            offset = type_key(b"yes\r")
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
    elif scenario in ("history-recover-cancel", "history-recover-stale", "history-recover-confirm"):
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
            offset = type_key(b"\r")
            outcome = "plan_stale" if scenario == "history-recover-stale" else "核对结果 已记录失败"
            expect(outcome, offset)
        type_key(b"q")
    elif scenario == "skill":
        expect("技能清单")
        expect("fixture-skill")
        original_count = len(capture)
        type_key(b"/@codex\r")
        expect("@codex", original_count)
        offset = type_key(b"\r")
        expect("仅手动触发", offset)
        type_key(b"\x03")
    elif scenario == "skill-visibility":
        expect("技能清单")
        type_key(b"/fixture-skill @cursor\r")
        expect("fixture-skill")
        offset = type_key(b"\r")
        expect("安装状态 在磁盘上", offset)
        expect("可发现 cursor:default · 共享目录", offset)
        expect("会话载入 未知", offset)
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
        expect("输入 yes", offset)
        if scenario == "skill-maintenance-stale":
            import json
            book = Path(target)
            changed = json.loads(book.read_text())
            changed["recipes"][0]["source"]["commit"] = "b" * 40
            book.write_text(json.dumps(changed))
        offset = type_key(b"n\r" if scenario == "skill-maintenance-cancel" else b"yes\r")
        result = {
            "skill-maintenance-cancel": "cancelled",
            "skill-maintenance-stale": "plan_stale",
            "skill-maintenance-removed": "卸载结果 succeeded",
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
    if not scenario.startswith(("setup-", "recommend-")):
        expect("\x1b[?1049l")
    actual_exit = process.wait(timeout=8)
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
