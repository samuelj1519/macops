#!/usr/bin/env python3
"""Drive a compiled macops binary against official chezmoi in synthetic contexts."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import time

NATIVE_SHA = '39d3cea7b9954ebbeddc7832edc7dd313a3902d72f468a4ab85ff1f2c2375a6d'


class Fixture:
    def __init__(self, root, macops, native, config_tail=''):
        self.root, self.macops = root, macops
        self.home = root / 'home'
        self.source = self.home / 'source'
        self.destination = self.home / 'destination'
        self.config = self.home / 'chezmoi.toml'
        for name in ['home/bin', 'home/source/home', 'home/destination', 'tmp', 'cwd', 'settings']:
            (root / name).mkdir(parents=True, exist_ok=False)
        shutil.copyfile(native, self.home / 'bin/chezmoi')
        (self.home / 'bin/chezmoi').chmod(0o755)
        (self.source / '.chezmoiroot').write_text('home\n')
        self.source_home = self.source / 'home'
        (self.source_home / 'dot_demo.tmpl').write_text('{{ .message }}\n')
        (self.source_home / 'dot_other').write_text('other\n')
        (self.source_home / 'dot_ignored').write_text('ignored\n')
        (self.source_home / '.chezmoiignore').write_text('{{ if eq .ignore "yes" }}.ignored{{ end }}\n')
        self.config.write_text('tempDir = ' + json.dumps(str(root / 'tmp')) + '\n[data]\nmessage = "native-value"\nignore = "yes"\n' + config_tail)
        self.env = {key: str(self.home / suffix) for key, suffix in [('HOME', ''), ('XDG_CONFIG_HOME', '.config'), ('XDG_DATA_HOME', '.local/share'), ('XDG_STATE_HOME', '.local/state'), ('XDG_CACHE_HOME', '.cache')]}
        self.env.update(PATH=str(self.home / 'bin') + ':/usr/bin:/bin', TMPDIR=str(root / 'tmp'), LC_ALL='C', LANG='C')
        self.base = [str(macops), '--json', '--home', str(self.home), '--state-dir', str(root / 'state'), '--cache-dir', str(root / 'cache'), '--settings', str(root / 'settings')]
        self.results = []

    def run(self, args, timeout=30):
        process = subprocess.run(self.base + list(map(str, args)), cwd=self.root / 'cwd', env=self.env, capture_output=True, timeout=timeout)
        try:
            value = json.loads(process.stdout)
        except json.JSONDecodeError:
            value = None
        self.results.append({'args': list(map(str, args)), 'exit': process.returncode, 'value': value, 'stderr': process.stderr.decode('utf-8', 'replace')})
        self.save()
        return process.returncode, value

    def enroll(self):
        return self.run(['--chezmoi-source', self.source, '--chezmoi-destination', self.destination, '--chezmoi-config', self.config, 'config', 'enroll'])

    def hook(self, body):
        script = self.root / 'apply-hook.sh'
        script.write_text('#!/bin/sh\nset -eu\n' + body + '\n')
        with self.config.open('a') as config:
            config.write('\n[hooks.apply.pre]\ncommand = "/bin/sh"\nargs = [' + json.dumps(str(script)) + ']\n')
        return script

    def save(self):
        (self.root / 'cli-results.json').write_text(json.dumps(self.results, indent=2) + '\n')


def group_alive(group):
    try:
        os.killpg(group, 0)
        return True
    except ProcessLookupError:
        return False


def wait_for(predicate, message, seconds=15):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(0.025)
    raise AssertionError(message)


def main(macops, native, output):
    output.mkdir(parents=True, exist_ok=False)
    assert hashlib.sha256(native.read_bytes()).hexdigest() == NATIVE_SHA
    binary_sha = hashlib.sha256(macops.read_bytes()).hexdigest()
    outcomes = []

    def check(name, passed, details):
        outcomes.append({'name': name, 'pass': bool(passed), 'details': details})

    f = Fixture(output / 'basic scope with spaces', macops, native)
    applied = f.destination / '.demo'
    marker = f.root / 'apply-sentinel'
    f.hook('printf "apply\\n" >> ' + shlex.quote(str(marker)))
    for args in [['--dry-run', 'config', 'enroll'], ['--dry-run', 'config', 'apply', applied, '--yes'], ['--dry-run', 'config', 'list', '--refresh'], ['--dry-run', 'config', 'verify', applied]]:
        code, value = f.run(args)
        check('dryrun_' + str(args[2]), code == 0 and value['data']['nativeInvocations'] == 0 and not marker.exists() and not (f.root / 'state').exists(), 'No Native command, state or cache initialization')
    check('enroll', f.enroll()[0] == 0, 'Explicit owner context; no per-command effect switch')
    check('native_effective_root', f.run(['config', 'source-path'])[1]['data']['path'] == str(f.source_home), '.chezmoiroot interpreted by Native')
    before = marker.read_text() if marker.exists() else ''
    f.run(['config', 'list'])
    f.run(['config', 'where', 'demo'])
    check('cached_views_no_hook', (marker.read_text() if marker.exists() else '') == before, 'list/where use registered metadata only')
    code, value = f.run(['config', 'apply', applied, '--yes'])
    check('native_selected_apply', code == 0 and value['data']['phase'] == 'succeeded' and applied.read_text() == 'native-value\n' and not (f.destination / '.other').exists(), 'Real Native template apply through compiled CLI; no unrelated selected target')
    check('native_verify', f.run(['config', 'verify', applied])[1]['data']['matched'] is True, 'Real Native verify and status')
    code, value = f.run(['config', 'list', '--refresh'])
    check('native_ignore', code == 0 and not any(str(path).endswith('.ignored') for path in value['data']['candidates']['shown']), 'Conditional ignore, not a Rust source parser')
    code, value = f.run(['config', 'verify', applied, f.destination / '.other'])
    check('multiple_selection_never_dropped', code == 3 and len(value['data']['targets']) == 2, 'Every explicitly selected target remains in scope')
    before = marker.read_text()
    code, _ = f.run(['config', 'apply', applied, f.destination / '.other', '--yes'])
    check('extra_apply_target_rejected', code != 0 and marker.read_text() == before, 'Single-target unit rejects extra operands without partial dispatch')
    (f.source_home / 'modify_dot_modified').write_text('#!/bin/sh\n/bin/cat > /dev/null\nprintf "modified\\n"\n')
    code, value = f.run(['config', 'apply', f.destination / '.modified', '--yes'])
    check('native_modify', code == 0 and (f.destination / '.modified').read_text() == 'modified\n', 'Native modify execution and idempotent verification')
    (f.source_home / 'symlink_dot_link').write_text('.demo')
    code, value = f.run(['config', 'apply', f.destination / '.link', '--yes'])
    check('native_symlink', code == 0 and (f.destination / '.link').is_symlink(), 'Native source attributes are not interpreted by Rust')
    applied.write_text('foreign\n')
    code, value = f.run(['config', 'apply', applied, '--yes'])
    check('yes_not_force', code == 3 and value['data']['phase'] == 'unknown' and applied.read_text() == 'foreign\n', 'Native destination conflict retained; no --force escalation')

    f = Fixture(output / 'managed-error0', macops, native)
    (f.source_home / 'create_dot_broken').write_text('not-a-directory\n')
    (f.source_home / 'dot_zzz').write_text('last\n')
    (f.destination / '.broken').mkdir()
    check('error0_owner_enroll', f.enroll()[0] == 0, 'Owner enrollment does not require full source body evaluation')
    code, value = f.run(['config', 'list', '--refresh'])
    check('managed_exit0_not_complete', code == 3 and value['error']['code'] == 'chezmoi_native_query_failed', 'Full status exposes create_ error after managed truncates exit0')
    code, value = f.run(['config', 'apply', f.destination / '.demo', '--yes'])
    check('selected_scope_not_full_source_gate', code == 0 and value['data']['phase'] == 'succeeded', 'An unrelated create_ failure does not block a valid selected target')

    f = Fixture(output / 'secret-and-modes', macops, native, 'dummySecret="SYNTHETIC_API_NEVER_PUBLIC"\n')
    (f.source_home / 'private_dot_secret.tmpl').write_text('{{ .dummySecret }}\n')
    (f.source_home / 'readonly_dot_readonly').write_text('readonly\n')
    (f.source_home / 'executable_dot_script').write_text('#!/bin/sh\nexit 0\n')
    f.hook('printf %s SYNTHETIC_API_NEVER_PUBLIC\nprintf %s SYNTHETIC_API_NEVER_PUBLIC >&2')
    f.enroll()
    for name, mode in [('.secret', 0o600), ('.readonly', 0o444), ('.script', 0o755)]:
        code, value = f.run(['config', 'apply', f.destination / name, '--yes'])
        check('native_mode_' + name, code == 0 and (f.destination / name).stat().st_mode & 0o777 == mode, 'Native owns private/readonly/executable permissions')
    f.run(['history'])
    public = json.dumps(f.results)
    retained = b''.join(p.read_bytes() for p in (f.root / 'state').rglob('*') if p.is_file() and p.name != 'chezmoi-native.boltdb')
    check('secret_not_public_or_retained_logs', 'SYNTHETIC_API_NEVER_PUBLIC' not in public and b'SYNTHETIC_API_NEVER_PUBLIC' not in retained, 'No raw Native stdout/stderr in public JSON, receipts, query audit or supervisor logs')

    f = Fixture(output / 'entry-filtered', macops, native, '\n[verify]\nexclude=["files"]\n')
    f.enroll()
    code, value = f.run(['config', 'apply', f.destination / '.demo', '--yes'])
    check('filtered_verify_is_partial', code == 3 and value['data']['phase'] == 'unknown' and value['data']['verification'] == 'partial-filtered', 'Native exit0 with configured entry exclusions cannot claim complete')

    f = Fixture(output / 'partial-effect', macops, native)
    side = f.destination / '.hook-effect'
    f.hook('printf effect > ' + shlex.quote(str(side)) + '\nexit 17')
    f.enroll()
    code, value = f.run(['config', 'apply', f.destination / '.demo', '--yes'])
    check('partial_effect_not_rolled_back', code == 3 and value['data']['phase'] == 'unknown' and side.read_text() == 'effect' and value['data']['rollback'] == 'not-provided', 'Native hook effect survives failure; no fabricated rollback')

    f = Fixture(output / 'source-cas', macops, native)
    f.hook('printf changed > ' + shlex.quote(str(f.source_home / 'dot_demo.tmpl')))
    f.enroll()
    code, value = f.run(['config', 'apply', f.destination / '.demo', '--yes'])
    check('selected_source_change_unknown', code == 3 and value['data']['phase'] == 'unknown' and value['data']['nativeResult'] == 'succeeded', 'Native succeeds after a source change, but frozen input verification refuses completion')

    f = Fixture(output / 'changing-helper', macops, native)
    helper = f.root / 'changing.sh'
    counter = f.root / 'counter'
    helper.write_text('#!/bin/sh\nif [ -f ' + shlex.quote(str(counter)) + ' ]; then n=$(/bin/cat ' + shlex.quote(str(counter)) + ');else n=0;fi\nn=$((n+1))\nprintf %s "$n" > ' + shlex.quote(str(counter)) + '\nprintf %s "$n"\n')
    (f.source_home / 'dot_demo.tmpl').write_text('{{ output "/bin/sh" ' + json.dumps(str(helper)) + ' }}\n')
    f.enroll()
    code, value = f.run(['config', 'apply', f.destination / '.demo', '--yes'])
    check('changing_helper_not_hermetic', code == 3 and value['data']['phase'] == 'unknown' and value['data']['nativeResult'] == 'succeeded' and value['data']['nativeExternalEffects'] == 'unverified', 'Changing helper output is detected by Native verify; stable source stamps never prove hermetic effects')

    f = Fixture(output / 'parent-kill', macops, native)
    started = f.root / 'started'
    f.hook('printf once\\n >> ' + shlex.quote(str(started)) + '\n/bin/sleep 1')
    f.enroll()
    parent = subprocess.Popen(f.base + ['config', 'apply', str(f.destination / '.demo'), '--yes'], cwd=f.root / 'cwd', env=f.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    wait_for(started.exists, 'Native apply hook never started')
    parent.kill()
    parent.communicate(timeout=5)
    journal = json.loads((f.root / 'state/active.json').read_text())
    original = journal['id']
    operation = json.loads((f.root / ('state/native-' + original + '-binding.json')).read_text())
    completion = f.root / 'state' / operation['completion_name']
    wait_for(lambda: completion.exists() and not group_alive(journal['process_group']), 'Original supervised Native did not finish', 15)
    code, value = f.run(['recover', original])
    check('parent_kill_original_completion', code == 0 and value['data']['phase'] == 'succeeded' and started.read_text().count('once') == 1, 'Original worker persists exit after parent dies; recovery does not apply twice')
    completion.unlink()
    for path in [f.root / 'state/active.json', f.root / ('state/operations/' + original + '.json')]:
        state = json.loads(path.read_text())
        state['phase'] = 'unknown'
        path.write_text(json.dumps(state))
    code, value = f.run(['recover', original])
    check('lost_completion_matching_files_unknown', code == 3 and value['data']['phase'] == 'unknown' and value['data']['filesMatch'] is True and started.read_text().count('once') == 1, 'Matching files never invent lost Native-success evidence')

    f = Fixture(output / 'timeout', macops, native)
    started = f.root / 'started'
    f.hook('printf once\\n >> ' + shlex.quote(str(started)) + '\n/bin/sleep 2')
    f.enroll()
    code, value = f.run(['--timeout-secs', '1', 'config', 'apply', f.destination / '.demo', '--yes'])
    check('timeout_unknown', code == 3 and value['data']['phase'] == 'unknown', 'Timeout remains unknown and never claims rollback')
    journal = json.loads((f.root / 'state/active.json').read_text())
    wait_for(lambda: not group_alive(journal['process_group']), 'Timed-out original group remains alive', 15)
    f.run(['recover', journal['id']])
    check('timeout_no_replay', started.read_text().count('once') == 1, 'Recovery queries original operation only')

    report = {'macops_sha256': binary_sha, 'chezmoi_sha256': NATIVE_SHA, 'outcomes': outcomes, 'all_pass': all(item['pass'] for item in outcomes)}
    assert hashlib.sha256(macops.read_bytes()).hexdigest() == binary_sha
    assert hashlib.sha256(native.read_bytes()).hexdigest() == NATIVE_SHA
    (output / 'summary.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2), flush=True)
    return 0 if report['all_pass'] else 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--macops', type=Path, required=True)
    parser.add_argument('--chezmoi', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    raise SystemExit(main(args.macops.resolve(), args.chezmoi.resolve(), args.output.resolve()))
