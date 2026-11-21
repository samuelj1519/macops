-------------------------- MODULE Operation --------------------------
EXTENDS Naturals, TLC
VARIABLES phase, dry, lock, journal, runs, target, observed, receipt, crashed, inFlight
vars == <<phase, dry, lock, journal, runs, target, observed, receipt, crashed, inFlight>>
Init == /\ phase = "planned" /\ dry \in BOOLEAN /\ lock = 0
        /\ journal = "none" /\ runs = 0 /\ target = "old"
        /\ observed = "none" /\ receipt = "old" /\ crashed = FALSE /\ inFlight = FALSE
Preview == /\ phase = "planned" /\ dry /\ phase' = "previewed" /\ UNCHANGED <<dry,lock,journal,runs,target,observed,receipt,crashed,inFlight>>
Acquire == /\ phase = "planned" /\ ~dry /\ lock = 0 /\ lock' = 1 /\ phase' = "locked" /\ UNCHANGED <<dry,journal,runs,target,observed,receipt,crashed,inFlight>>
Prepare == /\ phase = "locked" /\ phase' = "prepared" /\ journal' = "prepared" /\ UNCHANGED <<dry,lock,runs,target,observed,receipt,crashed,inFlight>>
Intent == /\ phase = "prepared" /\ phase' = "running" /\ journal' = "running" /\ UNCHANGED <<dry,lock,runs,target,observed,receipt,crashed,inFlight>>
Spawn == /\ phase = "running" /\ lock = 1 /\ journal = "running" /\ runs = 0 /\ runs' = 1 /\ inFlight' = TRUE /\ phase' = "executing" /\ UNCHANGED <<dry,lock,journal,target,observed,receipt,crashed>>
Complete == /\ inFlight /\ inFlight' = FALSE /\ target' \in {"old","new"} /\ UNCHANGED <<phase,dry,lock,journal,runs,observed,receipt,crashed>>
Collect == /\ phase = "executing" /\ ~inFlight /\ phase' = "checking" /\ UNCHANGED <<dry,lock,journal,runs,target,observed,receipt,crashed,inFlight>>
Observe == /\ phase \in {"checking","recovering"} /\ observed' \in {target,"unknown"} /\ phase' = "observed" /\ UNCHANGED <<dry,lock,journal,runs,target,receipt,crashed,inFlight>>
Commit == /\ phase = "observed" /\ observed = "new" /\ receipt' = "new" /\ phase' = "committed" /\ UNCHANGED <<dry,lock,journal,runs,target,observed,crashed,inFlight>>
Finish == /\ phase = "committed" /\ phase' = "succeeded" /\ journal' = "succeeded" /\ lock' = 0 /\ UNCHANGED <<dry,runs,target,observed,receipt,crashed,inFlight>>
Fail == /\ phase = "observed" /\ observed # "new" /\ phase' = (IF observed = "unknown" THEN "unknown" ELSE "failed") /\ journal' = phase' /\ lock' = 0 /\ UNCHANGED <<dry,runs,target,observed,receipt,crashed,inFlight>>
Crash == /\ ~crashed /\ phase \in {"locked","prepared","running","executing","checking","observed","committed"} /\ phase' = "interrupted" /\ lock' = 0 /\ crashed' = TRUE /\ UNCHANGED <<dry,journal,runs,target,observed,receipt,inFlight>>
Recover == /\ phase \in {"interrupted","unknown"} /\ ~inFlight /\ lock = 0 /\ lock' = 1 /\ phase' = "recovering" /\ UNCHANGED <<dry,journal,runs,target,observed,receipt,crashed,inFlight>>
AcquireOther == /\ lock = 0 /\ journal \notin {"prepared","running","unknown"} /\ phase \notin {"previewed","succeeded","failed","unknown"} /\ lock' = 2 /\ UNCHANGED <<phase,dry,journal,runs,target,observed,receipt,crashed,inFlight>>
ReleaseOther == /\ lock = 2 /\ lock' = 0 /\ UNCHANGED <<phase,dry,journal,runs,target,observed,receipt,crashed,inFlight>>
Terminal == phase \in {"previewed","succeeded","failed","unknown"}
Done == /\ Terminal /\ UNCHANGED vars
Advance == Preview \/ Acquire \/ Prepare \/ Intent \/ Spawn \/ Collect \/ Observe \/ Commit \/ Finish \/ Fail \/ Recover
Next == Advance \/ Complete \/ Crash \/ AcquireOther \/ ReleaseOther \/ Done
Spec == Init /\ [][Next]_vars /\ SF_vars(Advance) /\ WF_vars(ReleaseOther) /\ WF_vars(Complete)
PreviewDoesNotWrite == dry => /\ runs = 0 /\ journal = "none" /\ receipt = "old"
AtMostOnce == runs <= 1
IntentBeforeExecution == phase = "checking" => journal = "running"
VerifiedSuccess == phase = "succeeded" => /\ observed = "new" /\ receipt = "new"
ExclusiveExecution == phase \in {"locked","prepared","running","executing","checking","observed","committed","recovering"} => lock = 1
PendingBlocksOther == journal \in {"prepared","running","unknown"} => lock # 2
EventuallyTerminal == <>Terminal
=============================================================================
