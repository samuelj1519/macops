#!/usr/bin/env python3
"""Verify Native virtual external sources via a loopback-only synthetic server."""
import argparse
import hashlib
import http.server
import importlib.util
import json
from pathlib import Path
import threading

SPEC = importlib.util.spec_from_file_location('native_release_fixture', Path(__file__).with_name('verify_chezmoi_release.py'))
FIXTURES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FIXTURES)


def main(macops, native, output):
    output.mkdir(parents=True, exist_ok=False)
    assert hashlib.sha256(native.read_bytes()).hexdigest() == FIXTURES.NATIVE_SHA
    requests = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append(self.path)
            body = b'loopback-external\n'
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            pass

    server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        fixture = FIXTURES.Fixture(output / 'fixture', macops, native)
        (fixture.destination / '.nested').mkdir()
        (fixture.source_home / '.chezmoiexternal.toml').write_text('[".nested/external.conf"]\ntype="file"\nurl=' + json.dumps(f'http://127.0.0.1:{server.server_port}/value') + '\n')
        enrolled = fixture.enroll()
        target = fixture.destination / '.nested/external.conf'
        mapped = fixture.run(['config', 'source-path', target])
        applied = fixture.run(['config', 'apply', target, '--yes'])
        refreshed = fixture.run(['config', 'list', '--refresh'])
        listed = fixture.run(['list', '--kind', 'configs'])
        passed = (enrolled[0] == 0 and mapped[0] == 0 and applied[0] == 0 and applied[1]['data']['sourceObservation'] == 'native-virtual'
                  and target.read_text() == 'loopback-external\n' and refreshed[0] == 0
                  and any(row.get('target') == str(target) and row.get('owner') == 'chezmoi-native-virtual' for row in listed[1]['data']['resources'])
                  and requests == ['/value'])
        report = {'macops_sha256': hashlib.sha256(macops.read_bytes()).hexdigest(), 'chezmoi_sha256': FIXTURES.NATIVE_SHA, 'loopback_requests': requests, 'mapping': mapped, 'apply': applied, 'refresh': refreshed, 'resources': listed, 'destination_parent_preexists': True, 'all_pass': passed}
        (output / 'summary.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(report, indent=2))
        return 0 if passed else 1
    finally:
        server.shutdown()
        thread.join()
        server.server_close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--macops', type=Path, required=True)
    parser.add_argument('--chezmoi', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    raise SystemExit(main(args.macops.resolve(), args.chezmoi.resolve(), args.output.resolve()))
