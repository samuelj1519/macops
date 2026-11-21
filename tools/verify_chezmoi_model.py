#!/usr/bin/env python3
"""Verify cumulative Native RUN counting, semantic mutations and literal traces."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

TLC = Path(os.environ.get("TLA2TOOLS_JAR", "tla2tools.jar")).resolve()
WITNESSES = [
    'NoPreview', 'NoBlockedAuthorization', 'NoSuccess', 'NoNativePartial',
    'NoMismatch', 'NoUnknownFilesMatch', 'NoCrashRecovery', 'NoStaleBlock',
]


def replace_once(source, before, after):
    count = source.count(before)
    if count != 1:
        raise ValueError(f'mutation replacement must match exactly once; found {count}: {before!r}')
    return source.replace(before, after)


def violation(output):
    match = re.search(r'^Error: (Invariant|Action property) (\w+) is violated\.$', output, re.MULTILINE)
    return None if match is None else (match.group(1), match.group(2))


def literal_trace(output):
    states = []
    for match in re.finditer(r'^State (\d+): <([^>]+)>\n(.*?)(?=^State \d+:|\Z)', output, re.MULTILINE | re.DOTALL):
        values = {}
        for key, raw in re.findall(r'^/\\ (\w+) = ("[^"\n]*"|TRUE|FALSE|\d+)\s*$', match.group(3), re.MULTILINE):
            values[key] = raw == 'TRUE' if raw in ('TRUE', 'FALSE') else json.loads(raw)
        states.append({'number': int(match.group(1)), 'action': match.group(2).split()[0], 'values': values})
    return states


def witness_matches(name, states):
    if not states:
        return False
    value = states[-1]['values']
    predicates = {
        'NoPreview': lambda v: v['phase'] == 'previewed' and v['dry'] and v['runs'] == 0 and not v['queryRan'],
        'NoBlockedAuthorization': lambda v: v['phase'] == 'blocked' and not v['authorized'] and not v['dry'],
        'NoSuccess': lambda v: v['phase'] == 'complete' and v['nativeExit'] == 'ok' and v['verified'] and v['runs'] == 1,
        'NoNativePartial': lambda v: v['phase'] == 'partial' and v['partial'] and v['nativeExit'] == 'failed',
        'NoMismatch': lambda v: v['phase'] == 'unknown' and v['nativeExit'] == 'ok' and not v['verified'] and v['stable'],
        'NoUnknownFilesMatch': lambda v: v['phase'] == 'unknown' and v['nativeExit'] == 'unknown' and v['verified'] and v['recovered'],
        'NoCrashRecovery': lambda v: v['crashed'] and v['recovered'] and v['runs'] == 1,
        'NoStaleBlock': lambda v: v['phase'] == 'blocked' and not v['stable'],
    }
    try:
        return predicates[name](value) and sum(state['action'] == 'RunOnce' for state in states) == value['runs']
    except KeyError:
        return False


def replay_matches(states, expected_counter):
    runs = [index for index, state in enumerate(states) if state['action'] == 'RunOnce']
    if len(runs) != 2:
        return False
    last = runs[-1]
    return (last > 0 and states[last - 1]['values'].get('phase') == 'unknown'
            and states[last]['values'].get('phase') == 'running'
            and states[last]['values'].get('runs') == expected_counter)


def checks(source, config):
    cases = [('baseline', source, config, None, None)]
    cases.extend((name, source, config + '\nINVARIANT ' + name + '\n', ('Invariant', name), 'witness') for name in WITNESSES)
    dry = replace_once(source, '/\\ phase\' = "previewed"', '/\\ phase\' = "previewed" /\\ queryRan\' = TRUE')
    dry = replace_once(dry, 'stable, runs, queryRan, alive, nativeExit,\n                verified, crashed, recovered, partial>>\nBlockUnapproved', 'stable, runs, alive, nativeExit,\n                verified, crashed, recovered, partial>>\nBlockUnapproved')
    invented = replace_once(source, 'IF nativeExit = "ok" /\\ verified\' THEN "complete" ELSE "unknown"', 'IF verified\' THEN "complete" ELSE "unknown"')
    legacy = replace_once(source, '/\\ verified\' \\in BOOLEAN /\\ queryRan\' = TRUE /\\ recovered\' = TRUE', '/\\ verified\' \\in BOOLEAN /\\ queryRan\' = TRUE /\\ recovered\' = TRUE /\\ runs\' = runs + 1')
    legacy = replace_once(legacy, 'UNCHANGED <<dry, authorized, stable, runs, alive, nativeExit,\n                crashed, partial>>\nResolveFailed', 'UNCHANGED <<dry, authorized, stable, alive, nativeExit,\n                crashed, partial>>\nResolveFailed')
    cases.extend([
        ('dry_native', dry, config, ('Invariant', 'DryNoNative'), None),
        ('invent_exit', invented, config, ('Invariant', 'NoInventedSuccess'), None),
        ('legacy_replay_counter_increment', legacy, config, ('Invariant', 'AtMostOnce'), None),
    ])
    guard = replace_once(source, '/\\ phase = "prepared" /\\ stable /\\ runs = 0 /\\ authorized /\\ ~dry', '/\\ phase \\in {"prepared", "unknown"} /\\ stable /\\ authorized /\\ ~dry')
    invariant_config = replace_once(config, ' NoRepeatRunAction', '')
    action_config = replace_once(config, ' AtMostOnce', '')
    cases.extend([
        ('guard_only_replay', guard, config, ('Invariant', 'AtMostOnce'), 'replay2'),
        ('guard_replay_invariant_oracle', guard, invariant_config, ('Invariant', 'AtMostOnce'), 'replay2'),
        ('guard_replay_action_oracle', guard, action_config, ('Action property', 'NoRepeatRunAction'), 'replay2'),
    ])
    saturated = replace_once(guard, "runs' = runs + 1", "runs' = 1")
    cases.append(('saturated_counter_real_replay', saturated, config, ('Action property', 'NoRepeatRunAction'), 'replay1'))
    reset = replace_once(source, 'phase\' = "unknown" /\\ alive\' = FALSE /\\ nativeExit\' = "unknown"', 'phase\' = "unknown" /\\ alive\' = FALSE /\\ nativeExit\' = "unknown" /\\ runs\' = 0')
    reset = replace_once(reset, 'UNCHANGED <<dry, authorized, stable, runs, queryRan, verified,\n                crashed, recovered>>\nVerify', 'UNCHANGED <<dry, authorized, stable, queryRan, verified,\n                crashed, recovered>>\nVerify')
    cases.append(('reset_counter', reset, config, ('Action property', 'RunsNeverDecrease'), 'reset'))
    return cases


def run(root, output):
    model = root / 'docs/models/chezmoi_native/ChezmoiNative.tla'
    source, config = model.read_text(), model.with_suffix('.cfg').read_text()
    results = []
    for name, candidate, cfg, expected, oracle in checks(source, config):
        work = output / name
        work.mkdir()
        (work / model.name).write_text(candidate)
        (work / 'check.cfg').write_text(cfg)
        (work / 'tmp').mkdir()
        command = ['java', '-Djava.io.tmpdir=' + str(work / 'tmp'), '-XX:+UseParallelGC', '-cp', str(TLC), 'tlc2.TLC', '-workers', '1', '-metadir', str(work / 'states'), '-config', 'check.cfg', model.name]
        environment = dict(os.environ)
        for key in ['JAVA_TOOL_OPTIONS', '_JAVA_OPTIONS', 'JDK_JAVA_OPTIONS']:
            environment.pop(key, None)
        result = subprocess.run(command, cwd=work, env=environment, capture_output=True, text=True, timeout=60)
        text = result.stdout + result.stderr
        (work / 'run.log').write_text(text)
        actual, states = violation(text), literal_trace(text)
        passed = (result.returncode == 0 and 'Model checking completed. No error has been found.' in text) if expected is None else (result.returncode != 0 and actual == expected)
        if oracle == 'witness':
            passed = passed and witness_matches(name, states)
        elif oracle in ('replay1', 'replay2'):
            passed = passed and replay_matches(states, int(oracle[-1]))
        elif oracle == 'reset':
            passed = passed and any(left['values'].get('runs', 0) > right['values'].get('runs', 0) for left, right in zip(states, states[1:]))
        (work / 'literal-trace.json').write_text(json.dumps(states, indent=2) + '\n')
        results.append({'name': name, 'expected_violation': expected, 'actual_violation': actual, 'oracle': oracle, 'exit': result.returncode, 'run_actions': sum(state['action'] == 'RunOnce' for state in states), 'final_counter': states[-1]['values'].get('runs') if states else None, 'pass': passed})
    summary = {'model_sha256': hashlib.sha256(source.encode()).hexdigest(), 'results': results, 'all_pass': all(item['pass'] for item in results)}
    (output / 'results.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2))
    return 0 if summary['all_pass'] else 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--output', type=Path, required=True)
    arguments = parser.parse_args()
    arguments.output.mkdir(parents=True, exist_ok=False)
    raise SystemExit(run(arguments.root.resolve(), arguments.output.resolve()))
