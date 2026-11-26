#!/usr/bin/env python3
"""Build isolated semantic mutants and kill them through real Native CLI behavior."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess

SPEC = importlib.util.spec_from_file_location('native_release_fixture', Path(__file__).with_name('verify_chezmoi_release.py'))
FIXTURES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FIXTURES)

MUTATIONS = {
    'lost_native_exit': ('crates/app/src/config/chezmoi_native.rs', 'if known_success && verified == Some(true) && !operation.binding.filtered {', 'if verified == Some(true) && !operation.binding.filtered {'),
    'force_destination': ('crates/app/src/config/rules/chezmoi_native.rs', '&["--error-on-conflict", "apply", "--recursive=false"]', '&["--force", "apply", "--recursive=false"]'),
    'accept_source_change': ('crates/app/src/config/chezmoi_binding.rs', 'if inputs(binding, &tracked)? != binding.inputs {', 'if false && inputs(binding, &tracked)? != binding.inputs {'),
    'execute_dryrun': ('crates/app/src/config/chezmoi_native.rs', 'if dry {', 'if dry && false {'),
    'replay_unknown': ('crates/app/src/config/chezmoi_native.rs', '    if resolve_failed {\n        operations::confirm', '    if !resolve_failed {\n        let Some(QuerySpec::NativeConfig(query))=&journal.plan.query else{return Err(io::changed());};\n        let target=query.targets[0].to_str().ok_or_else(io::changed)?;\n        return apply(ctx,store,operation.binding.clone(),target,true);\n    }\n    if resolve_failed {\n        operations::confirm'),
}


def source_files(root):
    return sorted(path for path in root.rglob('*') if path.is_file() and not set(path.relative_to(root).parts) & {'artifacts', 'target', '.git', '__pycache__'})


def scenario(name, binary, native, output):
    f = FIXTURES.Fixture(output, binary, native)
    target = f.destination / '.demo'
    if name == 'execute_dryrun':
        code, value = f.run(['--dry-run', 'config', 'enroll'])
        return code == 0 and value['data']['nativeInvocations'] == 0 and not (f.root / 'state').exists(), {'exit': code, 'state_created': (f.root / 'state').exists()}
    if name == 'replay_unknown':
        f.hook('printf once\\n >> ' + FIXTURES.shlex.quote(str(f.root / 'apply-count')))
    f.enroll()
    if name == 'accept_source_change':
        f.hook('printf changed > ' + FIXTURES.shlex.quote(str(f.source_home / 'dot_demo.tmpl')))
        code, value = f.run(['config', 'apply', target, '--yes'])
        return code == 3 and value['data']['phase'] == 'unknown', value
    code, value = f.run(['config', 'apply', target, '--yes'])
    assert code == 0 and value['data']['phase'] == 'succeeded'
    if name == 'force_destination':
        target.write_text('foreign\n')
        code, value = f.run(['config', 'apply', target, '--yes'])
        return code == 3 and value['data']['phase'] == 'unknown' and target.read_text() == 'foreign\n', value
    original = value['data']['operation_id']
    state = f.root / 'state'
    binding = json.loads((state / ('native-' + original + '-binding.json')).read_text())
    (state / binding['completion_name']).unlink()
    for path in [state / 'active.json', state / ('operations/' + original + '.json')]:
        journal = json.loads(path.read_text())
        journal['phase'] = 'unknown'
        path.write_text(json.dumps(journal))
    code, value = f.run(['recover', original])
    correct = code == 3 and value['data']['phase'] == 'unknown' and value['data']['filesMatch'] is True
    if name == 'replay_unknown':
        correct = correct and (f.root / 'apply-count').read_text().count('once') == 1
    return correct, value


def main(root, native, baseline, output):
    output.mkdir(parents=True, exist_ok=False)
    assert hashlib.sha256(native.read_bytes()).hexdigest() == FIXTURES.NATIVE_SHA
    files = source_files(root)
    results = []
    for name, (relative, before, after) in MUTATIONS.items():
        positive, evidence = scenario(name, baseline, native, output / (name + '-baseline'))
        if not positive:
            raise AssertionError(f'baseline scenario failed: {name}: {evidence}')
        source = output / name / 'source'
        source.mkdir(parents=True)
        for path in files:
            destination = source / path.relative_to(root)
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, destination)
            destination.chmod(path.stat().st_mode & 0o7777)
        target = source / relative
        text = target.read_text()
        if text.count(before) != 1:
            raise AssertionError(f'{name}: mutation requires one exact match, got {text.count(before)}')
        target.write_text(text.replace(before, after))
        build = output / name / 'build'
        env = dict(os.environ, RUSTC_WRAPPER='', CARGO_TARGET_DIR=str(build))
        compiled = subprocess.run(['cargo', 'build', '--release', '--locked', '--offline'], cwd=source, env=env, capture_output=True, text=True, timeout=300)
        (output / name / 'build.log').write_text(compiled.stdout + compiled.stderr)
        if compiled.returncode != 0:
            raise AssertionError(f'mutant did not compile: {name}')
        binary = build / 'release/macops'
        preserved, evidence = scenario(name, binary, native, output / (name + '-mutant-run'))
        result = {'name': name, 'compiled': True, 'baseline_pass': positive, 'killed': not preserved, 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'patch': {'path': relative, 'before': before, 'after': after}, 'evidence': evidence}
        results.append(result)
        (output / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    print(json.dumps(results, indent=2))
    return 0 if all(item['compiled'] and item['baseline_pass'] and item['killed'] for item in results) else 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--chezmoi', type=Path, required=True)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    raise SystemExit(main(args.root.resolve(), args.chezmoi.resolve(), args.baseline.resolve(), args.output.resolve()))
