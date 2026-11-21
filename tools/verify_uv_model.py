"""Check the uv observation model, reachability, and guard regressions."""
import os
import pathlib
import subprocess
import tempfile

BASE = pathlib.Path(__file__).resolve().parent.parent
MODEL = BASE / 'docs/models/uv_observed'
ART = BASE / 'artifacts/uv-readonly/model-checks'
JAR = str(pathlib.Path(os.environ.get("TLA2TOOLS_JAR", "tla2tools.jar")).resolve())
ART.mkdir(parents=True, exist_ok=True)
source = (MODEL / 'UvObserved.tla').read_text()
config = (MODEL / 'UvObserved.cfg').read_text()


def run(name, text, cfg, expected=None):
    with tempfile.TemporaryDirectory(prefix='uv-model-', dir='/private/tmp') as temp:
        path = pathlib.Path(temp)
        (path / 'UvObserved.tla').write_text(text)
        (path / 'check.cfg').write_text(cfg)
        result = subprocess.run(['java', '-XX:+UseParallelGC', '-cp', JAR, 'tlc2.TLC', '-workers', '1', '-metadir', str(path / 'states'), '-config', 'check.cfg', 'UvObserved.tla'], cwd=path, capture_output=True, text=True)
        output = result.stdout + result.stderr
        (ART / (name + '.log')).write_text(output)
        (ART / (name + '.tla')).write_text(text)
        if expected is None:
            assert result.returncode == 0 and 'No error has been found' in output, (name, output)
        else:
            assert result.returncode != 0 and 'Invariant ' + expected + ' is violated' in output, (name, output)
        print(name, 'PASS' if expected is None else 'VALID VIOLATION', flush=True)


run('baseline', source, config)
for witness in ['NoAdoptSuccess', 'NoCrashRecovery', 'NoAliceReuse', 'NoBobReuse', 'NoCASUnknown', 'NoLateOwnerUnknown', 'NoLateSingletonUnknown', 'NoDoubleOwnerBlocked', 'NoReadSuccess']:
    # Checked safety invariants stay enabled for all witnesses; witness is additional.
    run('witness-' + witness, source, config + 'INVARIANT ' + witness + '\n', witness)

prefix, invariants = source.split('UvNativeRunsAlwaysZero ==', 1)
mutants = [
    ('native-write', 'UvNativeRunsAlwaysZero', prefix.replace(' \\/ request="write"', '').replace('receiptOp,nativeRuns,stateWrites,macopsBChanges,recovered,lock>>\nPreview', 'receiptOp,stateWrites,macopsBChanges,recovered,lock>>\n /\\ nativeRuns\'=(IF request="write" THEN 1 ELSE 0)\nPreview')),
    ('owner-conflict-gates-only', 'ConflictNeverProceeds', prefix.replace('Cardinality(owners)>1 \\/ ', '').replace(' /\\ Cardinality(owners)<=1', '')),
    ('owner-cas-only', 'FrozenOwnerCAS', prefix.replace(' /\\ owners=frozenOwners', '')),
    ('owner-singleton-cas-only', 'SingletonReplacementCAS', prefix.replace(' /\\ owners=frozenOwners', '')),
    ('home-identity', 'SameCanonicalIdentityAcrossViews', prefix.replace('<<"uv", r, "demo-tool">>', '<<"uv", "HOME", "demo-tool">>')),
    ('missing-adopt', 'AdoptOnlyStablePresent', prefix.replace('observation="present" /\\ ', '').replace(' /\\ observation="present"', '')),
    ('stamp-omitted', 'DriftBeforeCommitBlocksReceipt', prefix.replace(' /\\ stamp=frozen', '').replace('IF stamp=frozen /\\ ', 'IF ')),
    ('recover-spawn', 'RecoverNeverSpawns', prefix.replace(' /\\ phase\'=(IF receiptOp=journalOp', " /\\ nativeRuns'=1\n /\\ phase'=(IF receiptOp=journalOp").replace('journalOp,receiptOp,nativeRuns,stateWrites,macopsBChanges,lock>>', 'journalOp,receiptOp,stateWrites,macopsBChanges,lock>>')),
    ('recover-new-operation', 'CommitUsesOriginalOperation', prefix.replace('Recover == /\\ phase="crashed" /\\ recovered\'=TRUE', 'Recover == /\\ phase="crashed" /\\ recovered\'=TRUE /\\ journalOp\'=2').replace('observation,journalOp,receiptOp,nativeRuns,stateWrites,macopsBChanges,lock>>', 'observation,receiptOp,nativeRuns,stateWrites,macopsBChanges,lock>>')),
]
for name, invariant, mutant in mutants:
    assert mutant != prefix
    # Preserve complete original invariant and witness suffix. Check the intended
    # invariant first so overlapping valid safety failures cannot mask its oracle.
    cfg = 'SPECIFICATION Spec\nCHECK_DEADLOCK FALSE\nINVARIANT ' + invariant + '\n'
    run('mutant-' + name, mutant + 'UvNativeRunsAlwaysZero ==' + invariants, cfg, invariant)
