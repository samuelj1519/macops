--------------------------- MODULE AiUpgrade ---------------------------
EXTENDS Naturals, TLC

VARIABLES phase, dry, forceLive, lock, journal, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed
vars == <<phase, dry, forceLive, lock, journal, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

Init ==
    /\ phase = "planned"
    /\ dry \in BOOLEAN
    /\ forceLive \in BOOLEAN
    /\ lock = 0
    /\ journal = "none"
    /\ initialLive \in {"v1", "drifted"}
    /\ prevLive = "none"
    /\ sourcePin = "v1"
    /\ livePin = initialLive
    /\ otherKeysClean \in BOOLEAN
    /\ payloadValid \in BOOLEAN
    /\ revertOk \in BOOLEAN
    /\ sourceWriteOk \in BOOLEAN
    /\ runs = 0
    /\ crashed = FALSE

Preview ==
    /\ phase = "planned"
    /\ dry
    /\ phase' = "previewed"
    /\ UNCHANGED <<dry, forceLive, lock, journal, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

BlockPreUpgradeDrift ==
    /\ phase = "planned"
    /\ ~dry
    /\ ~forceLive
    /\ (livePin # sourcePin \/ ~otherKeysClean)
    /\ phase' = "drift_blocked"
    /\ UNCHANGED <<dry, forceLive, lock, journal, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

Acquire ==
    /\ phase = "planned"
    /\ ~dry
    /\ (forceLive \/ (livePin = sourcePin /\ otherKeysClean))
    /\ lock = 0
    /\ journal = "none"
    /\ lock' = 1
    /\ phase' = "locked"
    /\ UNCHANGED <<dry, forceLive, journal, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

Prepare ==
    /\ phase = "locked"
    /\ prevLive' = livePin
    /\ journal' = "prepared"
    /\ phase' = "prepared"
    /\ UNCHANGED <<dry, forceLive, lock, initialLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

RunMisePin ==
    /\ phase = "prepared"
    /\ runs = 0
    /\ runs' = 1
    /\ journal' = "running"
    /\ livePin' = "v2"
    /\ phase' = "upgraded_live"
    /\ UNCHANGED <<dry, forceLive, lock, initialLive, prevLive, sourcePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, crashed>>

CheckPayloadGate ==
    /\ phase \in {"upgraded_live", "recovering_gate"}
    /\ phase' = (IF payloadValid THEN "gate_passed" ELSE "gate_failed")
    /\ UNCHANGED <<dry, forceLive, lock, journal, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

ReconcileSourcePinOk ==
    /\ phase = "gate_passed"
    /\ ~forceLive
    /\ sourceWriteOk
    /\ sourcePin' = "v2"
    /\ journal' = "succeeded"
    /\ phase' = "succeeded"
    /\ lock' = 0
    /\ UNCHANGED <<dry, forceLive, initialLive, prevLive, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

ReconcileSourcePinFail ==
    /\ phase = "gate_passed"
    /\ ~forceLive
    /\ ~sourceWriteOk
    /\ journal' = "unknown"
    /\ phase' = "unknown"
    /\ lock' = 0
    /\ UNCHANGED <<dry, forceLive, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

FinishForceLive ==
    /\ phase = "gate_passed"
    /\ forceLive
    /\ journal' = "succeeded"
    /\ phase' = "succeeded"
    /\ lock' = 0
    /\ UNCHANGED <<dry, forceLive, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

RevertLivePinSuccess ==
    /\ phase = "gate_failed"
    /\ revertOk
    /\ prevLive # "none"
    /\ livePin' = prevLive
    /\ journal' = "failed"
    /\ phase' = "failed"
    /\ lock' = 0
    /\ UNCHANGED <<dry, forceLive, initialLive, prevLive, sourcePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

RevertLivePinFailure ==
    /\ phase = "gate_failed"
    /\ ~revertOk
    /\ journal' = "unknown"
    /\ phase' = "unknown"
    /\ lock' = 0
    /\ UNCHANGED <<dry, forceLive, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

Crash ==
    /\ ~crashed
    /\ phase \in {"locked", "prepared", "upgraded_live", "gate_passed", "gate_failed"}
    /\ phase' = (IF journal = "none" THEN "failed" ELSE "interrupted")
    /\ lock' = 0
    /\ crashed' = TRUE
    /\ UNCHANGED <<dry, forceLive, journal, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs>>

RecoverAcquire ==
    /\ phase = "interrupted"
    /\ lock = 0
    /\ lock' = 1
    /\ phase' = "recovering"
    /\ UNCHANGED <<dry, forceLive, journal, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

RecoverNoRun ==
    /\ phase = "recovering"
    /\ journal = "prepared"
    /\ runs = 0
    /\ journal' = "failed"
    /\ phase' = "failed"
    /\ lock' = 0
    /\ UNCHANGED <<dry, forceLive, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

RecoverEvaluate ==
    /\ phase = "recovering"
    /\ ~(journal = "prepared" /\ runs = 0)
    /\ phase' = "recovering_gate"
    /\ UNCHANGED <<dry, forceLive, lock, journal, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

OtherAcquire ==
    /\ lock = 0
    /\ journal \notin {"prepared", "running", "unknown"}
    /\ lock' = 2
    /\ UNCHANGED <<phase, dry, forceLive, journal, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

OtherRelease ==
    /\ lock = 2
    /\ lock' = 0
    /\ UNCHANGED <<phase, dry, forceLive, journal, initialLive, prevLive, sourcePin, livePin, otherKeysClean, payloadValid, revertOk, sourceWriteOk, runs, crashed>>

Terminal == phase \in {"previewed", "drift_blocked", "succeeded", "failed", "unknown"}
Done == /\ Terminal /\ lock = 0 /\ UNCHANGED vars

Progress ==
    \/ Preview
    \/ BlockPreUpgradeDrift
    \/ Acquire
    \/ Prepare
    \/ RunMisePin
    \/ CheckPayloadGate
    \/ ReconcileSourcePinOk
    \/ ReconcileSourcePinFail
    \/ FinishForceLive
    \/ RevertLivePinSuccess
    \/ RevertLivePinFailure
    \/ RecoverAcquire
    \/ RecoverNoRun
    \/ RecoverEvaluate

Next ==
    \/ Progress
    \/ Crash
    \/ OtherAcquire
    \/ OtherRelease
    \/ Done

Spec == Init /\ [][Next]_vars /\ SF_vars(Progress) /\ WF_vars(OtherRelease)

PreviewDoesNotWrite ==
    dry => /\ runs = 0 /\ journal = "none" /\ sourcePin = "v1" /\ livePin = initialLive

PreDriftBlocksWithoutForceLive ==
    (~forceLive /\ (initialLive # "v1" \/ ~otherKeysClean)) => (runs = 0 /\ sourcePin = "v1" /\ livePin = initialLive)

ForceLiveNeverWritesSource ==
    forceLive => sourcePin = "v1"

FailedPayloadNeverAdvancesSource ==
    ~payloadValid => sourcePin = "v1"

RevertRestoresPriorLive ==
    (~payloadValid /\ revertOk /\ runs = 1 /\ phase = "failed") => (livePin = prevLive /\ prevLive = initialLive)

SourceReconcileFailureBlocksUnknown ==
    (payloadValid /\ ~forceLive /\ ~sourceWriteOk /\ runs = 1 /\ Terminal) => (phase = "unknown" /\ journal = "unknown" /\ sourcePin = "v1")

SucceededRequiresValidPayload ==
    phase = "succeeded" =>
        /\ payloadValid
        /\ livePin = "v2"
        /\ (~forceLive => sourcePin = "v2")

RevertFailureBlocksUnknown ==
    (~payloadValid /\ ~revertOk /\ runs = 1 /\ Terminal) => (phase = "unknown" /\ journal = "unknown")

AtMostOnce ==
    runs <= 1

PendingBlocksOther ==
    journal \in {"prepared", "running", "unknown"} => lock # 2

ExclusiveExecution ==
    phase \in {"locked", "prepared", "upgraded_live", "gate_passed", "gate_failed", "recovering", "recovering_gate"} => lock = 1

EventuallyTerminal == <>Terminal

NoReconciledUpgradeSuccess == ~(phase = "succeeded" /\ ~forceLive /\ sourcePin = "v2")
NoGateRevertDriftedForceLive == ~(phase = "failed" /\ forceLive /\ initialLive = "drifted" /\ livePin = "drifted" /\ runs = 1)
NoSourceWriteFailUnknown == ~(phase = "unknown" /\ payloadValid /\ ~forceLive /\ ~sourceWriteOk /\ runs = 1)
NoDriftBlocked == ~(phase = "drift_blocked")
=============================================================================
