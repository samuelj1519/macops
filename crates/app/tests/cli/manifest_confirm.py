import json
from pathlib import Path
import os
import pty
import select
import subprocess
import sys
import time

catalog = Path(sys.argv[1])
argv = sys.argv[2:]
master, tty = pty.openpty()
child = subprocess.Popen(argv, stdin=tty, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
os.close(tty)
try:
    deadline = time.monotonic() + 12
    received = bytearray()
    while b'yes' not in received:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise AssertionError('confirmation not observed')
        readable, _, _ = select.select([child.stderr], [], [], remaining)
        if not readable:
            raise AssertionError('confirmation exceeded deadline')
        block = os.read(child.stderr.fileno(), 4096)
        if not block:
            raise AssertionError(f'child exited before confirmation: {child.poll()}')
        received.extend(block)
    catalog.write_text('{"schema_version":1,"packages":[]}')
    os.write(master, b'yes\n')
    out, _err = child.communicate(timeout=10)
    data = json.loads(out)
    assert child.returncode == 3, data
    assert data.get('error', {}).get('code') == 'manifest_recipe_required', data
finally:
    os.close(master)
    if child.poll() is None:
        child.kill()
        child.wait()
