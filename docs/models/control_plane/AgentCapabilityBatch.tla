----------------------- MODULE AgentCapabilityBatch -----------------------
EXTENDS Naturals, FiniteSets

Agents == {1, 2, 3}
Units == {1, 2}
Consumers(u) == IF u = 1 THEN {1, 2} ELSE {3}
VARIABLES selected, initial, state, attempts, active, finished, authorized
vars == <<selected, initial, state, attempts, active, finished, authorized>>

Covered(u) == Consumers(u) \subseteq selected
Work(u) == Covered(u) /\ state[u] = "old" /\ attempts[u] < 2

Init ==
    /\ selected \in (SUBSET Agents) \ {{}}
    /\ state \in [Units -> {"old", "current"}]
    /\ initial = state
    /\ attempts = [u \in Units |-> 0]
    /\ active = 0
    /\ finished = FALSE
    /\ authorized = FALSE

Confirm ==
    /\ ~finished /\ ~authorized
    /\ authorized' = TRUE
    /\ UNCHANGED <<selected, initial, state, attempts, active, finished>>

Cancel ==
    /\ ~finished /\ ~authorized
    /\ finished' = TRUE
    /\ UNCHANGED <<selected, initial, state, attempts, active, authorized>>

Start(u) ==
    /\ authorized /\ ~finished /\ active = 0 /\ Work(u)
    /\ active' = u
    /\ state' = [state EXCEPT ![u] = "pending"]
    /\ attempts' = [attempts EXCEPT ![u] = @ + 1]
    /\ UNCHANGED <<selected, initial, finished, authorized>>

Observe ==
    /\ active # 0
    /\ \E result \in {"current", "old", "blocked"}:
        state' = [state EXCEPT ![active] = result]
    /\ active' = 0
    /\ UNCHANGED <<selected, initial, attempts, finished, authorized>>

Finish ==
    /\ authorized /\ ~finished /\ active = 0 /\ \A u \in Units: ~Work(u)
    /\ finished' = TRUE
    /\ UNCHANGED <<selected, initial, state, attempts, active, authorized>>

Next == Confirm \/ Cancel \/ (\E u \in Units: Start(u)) \/ Observe \/ Finish
        \/ (finished /\ UNCHANGED vars)

ApprovedScope == \A u \in Units: attempts[u] > 0 => (authorized /\ Covered(u))
OneWriter == {u \in Units: state[u] = "pending"} = ({active} \cap Units)
CanceledDoesNotWrite == (finished /\ ~authorized) => \A u \in Units: attempts[u] = 0
FinishedHasNoWriter == finished => active = 0
AlreadyCurrentDoesNotWrite == \A u \in Units: initial[u] = "current" => attempts[u] = 0
=============================================================================
