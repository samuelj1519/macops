"""Exhaustively check local Codex plugin re-add and recovery."""

import sys
from collections import deque
from dataclasses import dataclass, replace
from itertools import product


@dataclass(frozen=True)
class Admission:
    supported_binary: bool
    same_source: bool
    configured_enabled: bool
    isolated_storage: bool
    no_customization: bool
    input_bound: bool

    def permitted(self):
        return all(vars(self).values())


@dataclass(frozen=True)
class State:
    phase: str = "clear"
    calls: int = 0
    hold: bool = False
    exit_proven: bool = False
    passive_after: bool = False
    proof_owner: str | None = None
    drift: str | None = None


def steps(state, admission, mutation):
    yield "implicit_query", state
    if state.phase == "clear" and admission.permitted():
        yield "freeze_with_lock", replace(state, phase="intent", hold=True)
    if state.phase == "intent":
        yield "drift_in_locked_guard", replace(state, phase="unknown", hold=False)
        yield "native_launch", replace(state, phase="running", calls=state.calls + 1)
    if state.phase == "running":
        yield "parent_lost", replace(state, phase="unknown", hold=True)
        yield "native_exited", replace(state, phase="after", exit_proven=True)
        yield "native_uncertain", replace(state, phase="unknown", hold=False)
    if state.phase == "after":
        yield "worker_after_verified", replace(
            state, phase="unknown", passive_after=True, proof_owner="original", hold=False
        )
        yield "drift_partial_or_probe_failed", replace(state, phase="unknown", hold=False)
        yield "parent_lost", replace(state, phase="unknown", hold=True)
    if state.phase == "unknown" and state.hold:
        yield "runner_still_running", state
        yield "runner_disappeared", replace(state, hold=False)
        yield "original_exit_arrived", replace(state, exit_proven=True)
        if state.exit_proven:
            for owner in ("original", "different"):
                yield "after_proof_arrived", replace(state, passive_after=True, proof_owner=owner)
    if state.phase == "unknown" and not state.hold:
        if state.passive_after and state.drift is None:
            for part in ("scope", "selected", "unrelated"):
                yield f"drift_{part}", replace(state, drift=part)
        ready = state.exit_proven and state.passive_after and state.proof_owner == "original"
        matches_current = state.drift is None
        if (ready or mutation == "evidence") and (matches_current or mutation == "current"):
            yield "locked_reobserve_and_settle", replace(state, phase="succeeded")


def explore(admission, mutation):
    queue = deque([State()])
    seen = set(queue)
    transitions = 0
    while queue:
        state = queue.popleft()
        for event, other in steps(state, admission, mutation):
            transitions += 1
            assert other.calls <= 1
            if not admission.permitted():
                assert other.calls == 0 and not other.hold
            if state.phase == "unknown":
                assert other.calls == state.calls
            if event == "native_launch":
                assert state.phase == "intent" and state.hold
            if event == "implicit_query":
                assert state == other
            if other.phase == "succeeded":
                assert other.calls == 1 and other.exit_proven and other.passive_after
                assert other.proof_owner == "original"
                assert other.drift is None, f"stale {other.drift} evidence"
            if other not in seen:
                seen.add(other)
                queue.append(other)
    if admission.permitted():
        assert any(s.phase == "succeeded" for s in seen)
        assert any(s.phase == "unknown" and s.calls == 1 and s.hold for s in seen)
        assert any(s.phase == "unknown" and s.calls == 0 for s in seen)
        assert any(s.phase == "unknown" and s.proof_owner == "different" for s in seen)
    return len(seen), transitions


def replay_recovery_witness(mutation):
    admitted = Admission(True, True, True, True, True, True)
    state = State()
    path = [
        "freeze_with_lock",
        "native_launch",
        "parent_lost",
        "original_exit_arrived",
        "after_proof_arrived",
        "runner_disappeared",
    ]
    for event in path:
        state = next(
            after
            for action, after in steps(state, admitted, mutation)
            if action == event and after.proof_owner != "different"
        )
    settled = dict(steps(state, admitted, mutation))["locked_reobserve_and_settle"]
    assert settled.phase == "succeeded" and settled.calls == 1
    print("PASS original proof and current matching projection settle with one Native call")
    for part in ("scope", "selected", "unrelated"):
        stale = dict(steps(state, admitted, mutation))[f"drift_{part}"]
        assert stale.phase == "unknown" and stale.calls == 1
        choices = dict(steps(stale, admitted, mutation))
        assert "locked_reobserve_and_settle" not in choices, f"stale {part} evidence"
        print(f"PASS {part} drift stays Unknown with one Native call")


def main():
    mutation = {
        ("--bypass-recovery-evidence",): "evidence",
        ("--bypass-current-projection",): "current",
    }.get(tuple(sys.argv[1:]))
    replay_recovery_witness(mutation)
    answers = [explore(Admission(*flags), mutation) for flags in product((False, True), repeat=6)]
    print(
        f"PASS {len(answers)} admissions, {sum(a for a, _ in answers)} states, "
        f"{sum(b for _, b in answers)} transitions"
    )


if __name__ == "__main__":
    main()
