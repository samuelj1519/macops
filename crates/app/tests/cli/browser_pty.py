"""Drive the compiled CLI on a disposable terminal using synthetic profiles."""
import errno
import fcntl
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
    if scenario == "skills-discovered":
        expect("fixture-skill")
        assert b"fixture-loose" not in capture
        offset = type_key(b"a")
        expect("loose", offset)
        type_key(b"/fixture-loose\r")
        offset = type_key(b"\r")
        expect("载荷", offset)
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
    expect("\x1b[?1049l")
    assert process.wait(timeout=8) == int(exit_code)
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
