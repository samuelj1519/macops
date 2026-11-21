#!/usr/bin/env python3
"""Check SourceUpgrade, its required traces, and deliberate protocol mistakes."""
import os
import argparse
import json
from pathlib import Path
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
parser.add_argument('--output', type=Path)
parser.add_argument('--jar', type=Path, default=Path(os.environ.get("TLA2TOOLS_JAR", "tla2tools.jar")).resolve())
args = parser.parse_args()
root = args.root.resolve()
model = root / 'docs/models/source_upgrade'
original = (model / 'SourceUpgrade.tla').read_text()
config = (model / 'SourceUpgrade.cfg').read_text()
logs = args.output or root / 'artifacts/source-delivery/model-validation'
logs.mkdir(parents=True, exist_ok=True)
results = []

with tempfile.TemporaryDirectory(prefix='macops-source-model-', dir='/private/tmp') as temporary:
    scratch = Path(temporary)

    def run(name, source, cfg, expected_exit, expected_text):
        directory = scratch / name
        directory.mkdir()
        (directory / 'SourceUpgrade.tla').write_text(source)
        (directory / 'SourceUpgrade.cfg').write_text(cfg)
        command = ['java', '-XX:+UseParallelGC', '-cp', str(args.jar), 'tlc2.TLC', '-workers', '1', '-metadir', str(directory / 'states'), '-config', 'SourceUpgrade.cfg', 'SourceUpgrade.tla']
        process = subprocess.run(command, cwd=directory, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        (logs / (name + '.log')).write_text(process.stdout)
        matched = process.returncode == expected_exit and expected_text in process.stdout
        results.append({'name': name, 'exit': process.returncode, 'expected_exit': expected_exit, 'expected': expected_text, 'matched': matched})
        print(json.dumps(results[-1]), flush=True)
        return matched

    def changed(needle, replacement, source=original):
        if source.count(needle) != 1:
            raise RuntimeError('mutation anchor changed: ' + needle)
        return source.replace(needle, replacement)

    run('baseline', original, config, 0, 'No error has been found.')
    witnesses = ['NoUpgradeSuccess', 'NoAdoptionSuccess', 'NoNativePartialRecovery', 'NoOwnRestoredBefore', 'NoCrashAfterClearSuccess', 'NoForeignRootUnknown', 'NoUnknownInodeBlocked', 'NoUnchangedLedgerSuccess', 'NoLateCASForeignRetained', 'NoAdoptPartialRollback', 'NoRepeatedOwnRestoreRecovery', 'NoUpgradeAfterClearRecovery', 'NoDeclaredWithoutReceiptSuccess']
    for witness in witnesses:
        run('witness-' + witness, original, 'SPECIFICATION Spec\nINVARIANT ' + witness + '\n', 12, 'Invariant ' + witness + ' is violated.')

    bad = changed('!.evidence = TRUE', '!.evidence = FALSE')
    bad = changed('/\\ s.pending /\\ s.evidence /\\ s.intent = r', '/\\ s.pending /\\ s.intent = r', bad)
    run('mutant-unrecorded-candidate', bad, config, 12, 'Invariant CandidateRecordedBeforePublication is violated.')

    bad = changed('/\\ (k \\in Missing(s.scenario) \\/ k \\in s.savedRollback)', '/\\ TRUE')
    run('mutant-unrecorded-restored-inode', bad, config, 12, 'Invariant RestoredInodeWasRecorded is violated.')

    start = original.index('BeforeLedger(k) ==')
    end = original.index('AfterLedger(k) ==', start)
    bad = original[:start] + 'BeforeLedger(k) == s.ledgers[k].value = BeforeValue(k)\n' + original[end:]
    run('mutant-accept-foreign-same-bytes', bad, config, 12, 'Invariant NoExternalSuccess is violated.')

    bad = changed('AllAfter == (\\A r \\in Roots : AfterRoot(r)) /\\ (\\A k \\in Ledgers : AfterLedger(k))', 'AllAfter == (\\A r \\in Roots : AfterRoot(r)) /\\ (\\A k \\in Ledgers \\ {5} : AfterLedger(k))')
    run('mutant-skip-receipt-commit', bad, config, 12, 'Invariant ReceiptCommitComplete is violated.')

    bad = changed('AllAfter == (\\A r \\in Roots : AfterRoot(r)) /\\ (\\A k \\in Ledgers : AfterLedger(k))', 'AllAfter == (\\A r \\in Roots : AfterRoot(r)) /\\ (\\A k \\in Ledgers \\ {1} : AfterLedger(k))')
    bad = changed('j < k => AfterLedger(j)', 'j < k /\\ j # 1 => AfterLedger(j)', bad)
    run('mutant-skip-native-commit', bad, config, 12, 'Invariant LiteralCommitPrefix is violated.')

    for skipped in [2, 3, 4]:
        before = 'AllAfter == (\\A r \\in Roots : AfterRoot(r)) /\\ (\\A k \\in Ledgers : AfterLedger(k))'
        after = 'AllAfter == (\\A r \\in Roots : AfterRoot(r)) /\\ (\\A k \\in Ledgers \\ {' + str(skipped) + '} : AfterLedger(k))'
        bad = changed(before, after)
        bad = changed('j < k => AfterLedger(j)', 'j < k /\\ j # ' + str(skipped) + ' => AfterLedger(j)', bad)
        run('mutant-skip-middle-ledger-' + str(skipped), bad, 'SPECIFICATION Spec\nINVARIANT LiteralLedgerCompletion\n', 12, 'Invariant LiteralLedgerCompletion is violated.')

    bad = changed('j < k => AfterLedger(j)', 'j < k /\\ k # 2 => AfterLedger(j)')
    run('mutant-definitions-before-native', bad, config, 12, 'Invariant LiteralCommitPrefix is violated.')

    start = original.index('SaveRollback(k) ==')
    end = original.index('RestoreRoot(r) ==', start)
    part = original[start:end]
    old = '(\\A j \\in Ledgers : j > k => BeforeLedger(j))'
    if part.count(old) != 2:
        raise RuntimeError('reverse order anchors changed')
    part = part.replace(old, 'TRUE')
    bad = original[:start] + part + original[end:]
    run('mutant-restore-native-too-early', bad, config, 12, 'Invariant LiteralCommitPrefix is violated.')

    at = original.index('Progress ==')
    bad = original[:at] + 'UnsafeCleanup == s.pending /\\ AllAfter /\\ Mode(s.scenario) = "upgrade" /\\ s\' = [s EXCEPT !.retained["alpha"] = None]\n' + original[at:]
    bad = changed('Next == Terminal \\/ Progress \\/ Crash', 'Next == UnsafeCleanup \\/ Terminal \\/ Progress \\/ Crash', bad)
    run('mutant-delete-retained-root', bad, config, 12, 'Invariant OriginalRootRetained is violated.')

    start = original.index('RestoreRoot(r) ==')
    end = original.index('VerifyBefore ==', start)
    part = original[start:end]
    part = changed('/\\ ~BeforeRoot(r) /\\ AfterRoot(r)', '/\\ r \\in s.selected /\\ Mode(s.scenario) = "upgrade"', part)
    bad = original[:start] + part + original[end:]
    run('mutant-repeat-restored-root-swap', bad, config, 13, 'Temporal properties were violated.')

    bad = changed('AllBefore == (\\A r \\in Roots : BeforeRoot(r)) /\\ (\\A k \\in Ledgers : BeforeLedger(k))', 'AllBefore == (\\A r \\in Roots : KnownRoot(r)) /\\ (\\A k \\in Ledgers : BeforeLedger(k))')
    start = bad.index('RestoreRoot(r) ==')
    end = bad.index('VerifyBefore ==', start)
    part = changed('~BeforeRoot(r) /\\ AfterRoot(r)', '~KnownRoot(r)', bad[start:end])
    bad = bad[:start] + part + bad[end:]
    run('mutant-known-after-root-as-restored', bad, config, 12, 'Invariant LiteralRollbackRootCompletion is violated.')

    start = original.index('RestoreLedger(k) ==')
    end = original.index('RestoreRoot(r) ==', start)
    part = original[start:end]
    part = changed('!.proof = None]', '!.proof = None, !.readSourceCalls = @ + 1]', part)
    bad = original[:start] + part + original[end:]
    run('mutant-refetch-during-recovery', bad, config, 12, 'Invariant RecoveryNeverRunsSource is violated.')

(logs / 'summary.json').write_text(json.dumps(results, indent=2) + '\n')
raise SystemExit(0 if all(result['matched'] for result in results) else 1)
