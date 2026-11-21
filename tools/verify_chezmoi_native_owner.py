#!/usr/bin/env python3
"""Compare native state/cache authority with compiled macops in isolated contexts."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess

MODULE = Path(__file__).with_name('verify_chezmoi_release.py')
SPEC = importlib.util.spec_from_file_location('native_release_fixture', MODULE)
FIXTURES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FIXTURES)
TEMPLATE = '{"configFile":{{ .chezmoi.configFile | toJson }},"cache":{{ .chezmoi.cacheDir | toJson }},"configuredState":{{ .chezmoi.config.persistentState | toJson }},"resolvedDefault":{{ joinPath (dir .chezmoi.configFile) "chezmoistate.boltdb" | toJson }}}'


def main(macops, native, output):
    output.mkdir(parents=True, exist_ok=False)
    assert hashlib.sha256(native.read_bytes()).hexdigest() == FIXTURES.NATIVE_SHA
    binary_sha = hashlib.sha256(macops.read_bytes()).hexdigest()
    results = []
    for case in ['default-search', 'custom-config', 'configured-state']:
        f = FIXTURES.Fixture(output / case, macops, native)
        if case == 'default-search':
            config = Path(f.env['XDG_CONFIG_HOME']) / 'chezmoi/chezmoi.toml'
            config.parent.mkdir(parents=True)
            config.write_text(f.config.read_text())
            config_args = []
        else:
            config = f.config
            config_args = ['--config', str(config)]
        if case == 'configured-state':
            owned = f.home / 'native-owned'
            owned.mkdir()
            config.write_text('persistentState=' + json.dumps(str(owned / 'state.boltdb')) + '\ncacheDir=' + json.dumps(str(owned / 'cache')) + '\n' + config.read_text())
        f.config = config
        base = [str(f.home / 'bin/chezmoi'), '--source', str(f.source), '--destination', str(f.destination)] + config_args
        metadata_result = subprocess.run(base + ['execute-template', TEMPLATE], cwd=f.root / 'cwd', env=f.env, capture_output=True, timeout=20)
        metadata = json.loads(metadata_result.stdout)
        target = f.destination / '.demo'
        seeded = subprocess.run(base + ['apply', '--recursive=false', '--', str(target)], cwd=f.root / 'cwd', env=f.env, capture_output=True, timeout=20)
        assert seeded.returncode == 0
        actual_state = Path(metadata['configuredState'] or metadata['resolvedDefault'])
        assert actual_state.is_file()
        target.write_text('foreign\n')
        native_status = subprocess.run(base + ['status', '--path-style=absolute', '--recursive=false', '--', str(target)], cwd=f.root / 'cwd', env=f.env, capture_output=True, timeout=20)
        assert native_status.returncode == 0
        enrollment = ['--chezmoi-source', f.source, '--chezmoi-destination', f.destination]
        if case != 'default-search':
            enrollment += ['--chezmoi-config', config]
        code, _ = f.run(enrollment + ['config', 'enroll'])
        assert code == 0
        code, status = f.run(['config', 'status', target])
        owner = json.loads((f.root / 'state/chezmoi-owner.json').read_text())
        correct = (owner['persistent_state'] == str(actual_state) and owner['cache'] == metadata['cache'] and code == 3 and status['data']['results'][0]['local'] == native_status.stdout.decode()[0])
        results.append({'case': case, 'native_metadata': metadata, 'native_created_state': str(actual_state), 'selected_state': owner['persistent_state'], 'selected_cache': owner['cache'], 'native_status': native_status.stdout.decode(), 'macops_status': status, 'pass': correct})
    report = {'macops_sha256': binary_sha, 'chezmoi_sha256': FIXTURES.NATIVE_SHA, 'cases': results, 'all_pass': all(case['pass'] for case in results)}
    (output / 'summary.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))
    return 0 if report['all_pass'] else 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--macops', type=Path, required=True)
    parser.add_argument('--chezmoi', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    raise SystemExit(main(args.macops.resolve(), args.chezmoi.resolve(), args.output.resolve()))
