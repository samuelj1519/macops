#!/usr/bin/env python3
"""Check bounded Brew identity/authority/once semantics using existing TLC only."""
import os
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

JAVA = Path(shutil.which(os.environ.get("JAVA", "java")) or "java")
JAR = Path(os.environ.get("TLA2TOOLS_JAR", "tla2tools.jar")).resolve()
ROOT = Path(__file__).resolve().parents[1]
MODEL = ROOT / 'docs/models/brew_native'
FIXTURES = ROOT / 'tools/fixtures/brew'


def replace_once(text, old, new):
    assert text.count(old) == 1, (old, text.count(old))
    return text.replace(old, new, 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'artifacts/brew-native/model-check')
    args = parser.parse_args()
    assert JAVA.is_file() and JAR.is_file(), 'existing Java/TLC required; no installation'
    args.output.mkdir(parents=True, exist_ok=False)
    launcher = args.output / 'launcher'
    launcher.mkdir()
    subprocess.run([str(JAVA.with_name('javac')), '-cp', str(JAR), '-d', str(launcher.resolve()),
                    str(FIXTURES / 'LoopbackOnlyTlc.java')], check=True)
    text = (MODEL / 'BrewNative.tla').read_text()
    cfg = (MODEL / 'BrewNative.cfg').read_text()
    results = []

    def run(name, source, config, violation=None, action=False):
        with tempfile.TemporaryDirectory(prefix='brew-native-tlc-', dir='/private/tmp') as tmp:
            work = Path(tmp)
            (work / 'BrewNative.tla').write_text(source)
            (work / 'check.cfg').write_text(config)
            command = [str(JAVA), '-XX:+UseParallelGC', '-Djava.io.tmpdir='+str(work), '-cp', str(launcher.resolve())+':'+str(JAR), 'LoopbackOnlyTlc',
                       '-workers', '1', '-metadir', str(work / 'states'), '-config', 'check.cfg', 'BrewNative.tla']
            result = subprocess.run(command, cwd=work, capture_output=True, text=True, timeout=60)
            output = result.stdout + result.stderr
            (args.output / (name + '.log')).write_text(output)
            (args.output / (name + '.tla')).write_text(source)
            (args.output / (name + '.cfg')).write_text(config)
            if violation:
                marker = ('Action property ' if action else 'Invariant ') + violation + ' is violated'
                assert result.returncode == (13 if action else 12) and marker in output, (name, result.returncode, marker, output)
                status = 'valid violation'
            else:
                assert result.returncode == 0 and 'No error has been found' in output, (name, output)
                status = 'PASS'
            counts = re.findall(r'([\d,]+) states generated, ([\d,]+) distinct states found', output)
            counts = counts[-1] if counts else None
            results.append({'name': name, 'status': status, 'exit': result.returncode,
                            'states': {'generated': int(counts[0].replace(',', '')), 'distinct': int(counts[1].replace(',', ''))} if counts else None,
                            'oracle': violation})
            print(name, status, flush=True)

    original_hashes = json.loads((FIXTURES / 'original-reviewer-inputs.json').read_text())
    for kind, oracle in [('program', 'FreshMissing'), ('cellar', 'FreshCellarMissing')]:
        folder = FIXTURES / ('original-reviewer-' + kind)
        for name, digest in original_hashes[kind].items():
            assert hashlib.sha256((folder / name).read_bytes()).hexdigest() == digest
        run('original-' + kind + '-freshness-red', (folder / 'BrewNative.tla').read_text(),
            (folder / 'check.cfg').read_text(), oracle)
        run('fixed-' + kind + '-same-oracle-green', text, (folder / 'check.cfg').read_text())
    run('baseline', text, cfg)
    for witness in ['NoWriterSuccess', 'NoRecoverySuccess', 'NoRebindSuccess', 'NoCellarRebind',
                    'NoLateDriftUnknown', 'NoConflictReject', 'NoTypedMissingNoop', 'NoDryPreview', 'NoBaseline', 'NoRefreshMissingProgram', 'NoRefreshMissingCellar', 'NoPendingFreshQuery']:
        run('witness-' + witness, text, cfg + '\nINVARIANT ' + witness + '\n', witness)

    def invariant_config(invariant):
        return 'SPECIFICATION Spec\nINVARIANT ' + invariant + '\n'

    owner_mut = replace_once(text, 'Owner(prefix, fullName, binding) == <<"formula", prefix, fullName>>',
                             'Owner(prefix, fullName, binding) == <<"formula", prefix, fullName, binding>>')
    run('mutant-binding-owner', owner_mut, invariant_config('StableOwner'), 'StableOwner')
    indirect = replace_once(text, 'Context(prefix, binding) == <<"brew", prefix>>',
                            'Context(prefix, binding) == <<"brew", prefix, binding>>')
    indirect = indirect.replace('Context(prefix, binding) == <<"brew", prefix, binding>>\n', '')
    indirect = replace_once(indirect, 'Owner(prefix, fullName, binding) == <<"formula", prefix, fullName>>',
                            'Context(prefix, binding) == <<"brew", prefix, binding>>\nOwner(prefix, fullName, binding) == <<"formula", Context(prefix,binding), fullName>>')
    run('mutant-indirect-binding-owner', indirect, invariant_config('StableOwner'), 'StableOwner')
    untrusted = replace_once(text, '!.trusted=TRUE, !.event="permit-query"', '!.trusted=FALSE, !.event="permit-query"')
    run('mutant-untrusted-query', untrusted, invariant_config('NoUntrustedQuery'), 'NoUntrustedQuery')
    no_scope = replace_once(text, 'IF s.trusted /\\ QueryFresh /\\ s.binding=s.frozen /\\ s.owners=1 /\\ ~s.overlap',
                            'IF s.trusted /\\ s.owners=1 /\\ ~s.overlap')
    run('mutant-worker-binding-gate', no_scope, invariant_config('NativeGate'), 'NativeGate')
    no_owner = replace_once(text, 'IF s.trusted /\\ QueryFresh /\\ s.binding=s.frozen /\\ s.owners=1 /\\ ~s.overlap',
                            'IF s.trusted /\\ QueryFresh /\\ s.binding=s.frozen /\\ ~s.overlap')
    run('mutant-worker-owner-gate', no_owner, invariant_config('NativeGate'), 'NativeGate')
    retarget = replace_once(text, 'Rebind == /\\ s.phase="bound"', 'Rebind == /\\ s.phase \\in {"bound", "unknown"}')
    run('mutant-rebind-pending', retarget, invariant_config('NoRetarget'), 'NoRetarget')
    retry = replace_once(text, '!.queryEpoch=s.epoch, !.queryValid=TRUE, !.recovered=TRUE, !.event="recover-query"',
                         '!.queryEpoch=s.epoch, !.queryValid=TRUE, !.runs=@+1, !.recovered=TRUE, !.event="recover-query"')
    run('mutant-recover-native-count', retry, invariant_config('NativeCount'), 'NativeCount')
    run('mutant-recover-native-norepeat', retry, 'SPECIFICATION Spec\nPROPERTY NoRepeat\n', 'NoRepeat', action=True)
    run('mutant-recover-native-action', retry, 'SPECIFICATION Spec\nPROPERTY RecoveryNoNative\n', 'RecoveryNoNative', action=True)
    no_proof = replace_once(text, 'Finish(post) == /\\ s.phase="running" /\\ post \\in {"complete", "incomplete", "unknown"}\n /\\ LET ok == post="complete"',
                            'Finish(post) == /\\ s.phase="running" /\\ post \\in {"complete", "incomplete", "unknown"}\n /\\ LET ok == TRUE')
    run('mutant-incomplete-success', no_proof, invariant_config('TypedCommit'), 'TypedCommit')
    stale_missing = replace_once(text, 'Noop == /\\ s.phase="bound" /\\ s.case="missing"\n /\\ LET ok == QueryFresh /\\ s.binding=s.frozen /\\ s.owners=1 /\\ ~s.overlap',
                                 'Noop == /\\ s.phase="bound" /\\ s.case="missing"\n /\\ LET ok == TRUE')
    run('mutant-stale-missing-noop', stale_missing, invariant_config('TypedCommit'), 'TypedCommit')
    selected = replace_once(text, 'IF QueryFresh /\\ Valid(s.case) /\\ s.selected=Key THEN "bound"', 'IF TRUE THEN "bound"')
    selected = replace_once(selected, '/\\ s.case="good" /\\ s.selected=Key /\\ s.runs=0', '/\\ s.runs=0')
    selected = replace_once(selected, 'Prepare == /\\ s.phase="bound" /\\ s.case="good"', 'Prepare == /\\ s.phase="bound"')
    run('mutant-wrong-selected-scope', selected, invariant_config('SelectedScope'), 'SelectedScope')
    fake_stamp = replace_once(text,
        '!.phase="refresh-required", !.frozen=s.binding, !.epoch=@+1,\n                    !.queryValid=FALSE,',
        '!.phase="bound", !.frozen=s.binding, !.epoch=@+1,\n                    !.queryBinding=s.binding, !.queryContext=Context("A",s.binding),\n                    !.queryEpoch=s.epoch+1, !.queryValid=TRUE,')
    run('mutant-rebind-forges-query-stamp', fake_stamp,
        'SPECIFICATION Spec\nPROPERTY EvidenceOrigin\n', 'EvidenceOrigin', action=True)
    summary = {'model_sha256': hashlib.sha256(text.encode()).hexdigest(),
               'config_sha256': hashlib.sha256(cfg.encode()).hexdigest(),
               'bounds': {'prefixes': 2, 'full_names': 3, 'bindings': 4, 'owners': 2, 'operations': 1},
               'native_counter': 'actual cumulative Naturals; no saturation or exploration count constraint',
               'results': results}
    (args.output / 'results.json').write_text(json.dumps(summary, indent=2) + '\n')


if __name__ == '__main__':
    main()
