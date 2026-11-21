---------------------------- MODULE service ----------------------------
EXTENDS Naturals, TLC
VARIABLES phase,dry,lock,journal,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent
vars == <<phase,dry,lock,journal,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
Init == /\ phase="planned" /\ dry \in BOOLEAN /\ lock=0 /\ journal="none" /\ runs=0
 /\ inFlight=FALSE /\ running \in BOOLEAN /\ loaded \in BOOLEAN /\ registered \in BOOLEAN /\ wanted \in BOOLEAN
 /\ obsR=running /\ obsL=loaded /\ obsReg=registered /\ crashed=FALSE /\ gateSent=FALSE
Preview == /\ phase="planned" /\ dry /\ phase'="previewed"
 /\ UNCHANGED <<dry,lock,journal,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
Noop == /\ phase="planned" /\ ~dry /\ running=wanted /\ loaded=wanted /\ registered=wanted /\ phase'="noop"
 /\ UNCHANGED <<dry,lock,journal,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
Acquire == /\ phase="planned" /\ ~dry /\ ~(running=wanted /\ loaded=wanted /\ registered=wanted) /\ lock=0
 /\ journal="none" /\ lock'=1 /\ phase'="locked"
 /\ UNCHANGED <<dry,journal,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
Prepare == /\ phase="locked" /\ journal'="prepared" /\ phase'="prepared"
 /\ UNCHANGED <<dry,lock,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
Intent == /\ phase="prepared" /\ journal'="running" /\ phase'="ready"
 /\ UNCHANGED <<dry,lock,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
Gate == /\ phase="ready" /\ gateSent'=TRUE /\ inFlight'=TRUE /\ runs'=runs+1 /\ phase'="executing"
 /\ UNCHANGED <<dry,lock,journal,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed>>
Complete == /\ inFlight /\ inFlight'=FALSE /\ running' \in BOOLEAN /\ loaded' \in BOOLEAN /\ registered' \in BOOLEAN
 /\ UNCHANGED <<phase,dry,lock,journal,runs,wanted,obsR,obsL,obsReg,crashed,gateSent>>
Observe == /\ phase="executing" /\ ~inFlight /\ phase'="observed" /\ obsR'=running /\ obsL'=loaded /\ obsReg'=registered
 /\ UNCHANGED <<dry,lock,journal,runs,inFlight,running,loaded,registered,wanted,crashed,gateSent>>
Finish == /\ phase="observed" /\ obsR=wanted /\ obsL=wanted /\ obsReg=wanted /\ journal'="succeeded" /\ phase'="succeeded" /\ lock'=0
 /\ UNCHANGED <<dry,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
Mismatch == /\ phase="observed" /\ ~(obsR=wanted /\ obsL=wanted /\ obsReg=wanted) /\ journal'="unknown" /\ phase'="unknown" /\ lock'=0
 /\ UNCHANGED <<dry,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
Crash == /\ ~crashed /\ phase \in {"locked","prepared","ready","executing","observed"}
 /\ phase'=(IF journal="none" THEN "failed" ELSE "interrupted") /\ lock'=0 /\ crashed'=TRUE
 /\ UNCHANGED <<dry,journal,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,gateSent>>
Recover == /\ phase="interrupted" /\ ~inFlight /\ lock=0 /\ lock'=1 /\ phase'="recovering"
 /\ UNCHANGED <<dry,journal,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
RecoverNoIntent == /\ phase="recovering" /\ journal="prepared" /\ phase'="failed" /\ journal'="failed" /\ lock'=0
 /\ UNCHANGED <<dry,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
RecoverQuery == /\ phase="recovering" /\ journal \in {"running","unknown"} /\ phase'="observed" /\ obsR'=running /\ obsL'=loaded /\ obsReg'=registered
 /\ UNCHANGED <<dry,lock,journal,runs,inFlight,running,loaded,registered,wanted,crashed,gateSent>>
Unknown == /\ phase="executing" /\ phase'="unknown" /\ journal'="unknown" /\ lock'=0
 /\ UNCHANGED <<dry,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
Recheck == /\ phase="unknown" /\ ~inFlight /\ lock=0 /\ phase'="recovering" /\ lock'=1
 /\ UNCHANGED <<dry,journal,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
OtherAcquire == /\ lock=0 /\ journal \notin {"prepared","running","unknown"} /\ phase="planned" /\ lock'=2
 /\ UNCHANGED <<phase,dry,journal,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
OtherRelease == /\ lock=2 /\ lock'=0
 /\ UNCHANGED <<phase,dry,journal,runs,inFlight,running,loaded,registered,wanted,obsR,obsL,obsReg,crashed,gateSent>>
Terminal == phase \in {"previewed","noop","succeeded","failed","unknown"}
Done == /\ Terminal /\ UNCHANGED vars
Progress == Preview \/ Noop \/ Acquire \/ Prepare \/ Intent \/ Gate \/ Observe \/ Finish \/ Mismatch \/ Recover \/ RecoverNoIntent \/ RecoverQuery
Next == Progress \/ Complete \/ Crash \/ Unknown \/ Recheck \/ OtherAcquire \/ OtherRelease \/ Done
Spec == Init /\ [][Next]_vars /\ SF_vars(Progress) /\ WF_vars(Complete) /\ WF_vars(OtherRelease)
PreviewDoesNotWrite == dry => /\ runs=0 /\ journal="none"
NoopDoesNotWrite == phase="noop" => /\ runs=0 /\ journal="none"
AtMostOnce == runs<=1
GateAfterDurableIntent == gateSent => journal \in {"running","unknown","succeeded","failed"}
VerifiedSuccess == phase="succeeded" => /\ obsR=wanted /\ obsL=wanted /\ obsReg=wanted
PendingBlocksOther == journal \in {"prepared","running","unknown"} => lock#2
ExclusiveExecution == phase \in {"locked","prepared","ready","executing","observed","recovering"} => lock=1
EventuallyTerminal == <>Terminal
NoStartSuccess == ~(phase="succeeded" /\ wanted)
NoStopSuccess == ~(phase="succeeded" /\ ~wanted)
NoPartialObservation == ~(phase="observed" /\ (obsR#obsL \/ obsL#obsReg))
NoRecoveredSuccess == ~(phase="succeeded" /\ crashed /\ gateSent)
NoUnregisteredObservation == ~(phase="observed" /\ obsR /\ obsL /\ ~obsReg /\ wanted)
FailedHasNoExecution == journal="failed" => runs=0
VerifiedNoop == phase="noop" => /\ running=wanted /\ loaded=wanted /\ registered=wanted
=============================================================================
