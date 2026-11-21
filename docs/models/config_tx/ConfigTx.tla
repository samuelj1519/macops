---------------------------- MODULE ConfigTx ----------------------------
EXTENDS Naturals, TLC

VARIABLES phase, dry, lock, journal, backup, target, obs, receipt, crashed, externalEdited
vars == <<phase, dry, lock, journal, backup, target, obs, receipt, crashed, externalEdited>>

Init ==
    /\ phase = "planned"
    /\ dry \in BOOLEAN
    /\ lock = 0
    /\ journal = "none"
    /\ backup = "none"
    /\ target \in {"pre", "post"}
    /\ obs = target
    /\ receipt = (IF target = "post" THEN "post" ELSE "old")
    /\ crashed = FALSE
    /\ externalEdited = FALSE

Preview ==
    /\ phase = "planned"
    /\ dry
    /\ phase' = "previewed"
    /\ UNCHANGED <<dry, lock, journal, backup, target, obs, receipt, crashed, externalEdited>>

Noop ==
    /\ phase = "planned"
    /\ ~dry
    /\ target = "post"
    /\ receipt = "post"
    /\ phase' = "noop"
    /\ UNCHANGED <<dry, lock, journal, backup, target, obs, receipt, crashed, externalEdited>>

Acquire ==
    /\ phase = "planned"
    /\ ~dry
    /\ ~(target = "post" /\ receipt = "post")
    /\ lock = 0
    /\ journal = "none"
    /\ lock' = 1
    /\ phase' = "locked"
    /\ UNCHANGED <<dry, journal, backup, target, obs, receipt, crashed, externalEdited>>

SaveBackupAndPrepare ==
    /\ phase = "locked"
    /\ target = "pre"
    /\ backup' = "saved"
    /\ journal' = "prepared"
    /\ phase' = "prepared"
    /\ UNCHANGED <<dry, lock, target, obs, receipt, crashed, externalEdited>>

BeginMutate ==
    /\ phase = "prepared"
    /\ backup = "saved"
    /\ journal' = "mutating"
    /\ phase' = "mutating"
    /\ UNCHANGED <<dry, lock, backup, target, obs, receipt, crashed, externalEdited>>

WriteTargetCasOk ==
    /\ phase = "mutating"
    /\ target = "pre"
    /\ target' \in {"post", "bad"}
    /\ phase' = "written"
    /\ UNCHANGED <<dry, lock, journal, backup, obs, receipt, crashed, externalEdited>>

AbortBeforeWriteOnCasMismatch ==
    /\ phase = "mutating"
    /\ target # "pre"
    /\ journal' = "unknown"
    /\ phase' = "unknown"
    /\ lock' = 0
    /\ UNCHANGED <<dry, backup, target, obs, receipt, crashed, externalEdited>>

PostVerify ==
    /\ phase \in {"written", "recovering_check"}
    /\ obs' = target
    /\ phase' = "verified"
    /\ UNCHANGED <<dry, lock, journal, backup, target, receipt, crashed, externalEdited>>

CommitReceipt ==
    /\ phase = "verified"
    /\ obs = "post"
    /\ target = "post"
    /\ receipt' = "post"
    /\ phase' = "committed"
    /\ UNCHANGED <<dry, lock, journal, backup, target, obs, crashed, externalEdited>>

FinishSuccess ==
    /\ phase = "committed"
    /\ journal' = "succeeded"
    /\ phase' = "succeeded"
    /\ lock' = 0
    /\ UNCHANGED <<dry, backup, target, obs, receipt, crashed, externalEdited>>

RollbackOnBadCasOk ==
    /\ phase = "verified"
    /\ obs = "bad"
    /\ target = "bad"
    /\ backup = "saved"
    /\ target' = "pre"
    /\ journal' = "restored"
    /\ phase' = "failed"
    /\ lock' = 0
    /\ UNCHANGED <<dry, backup, obs, receipt, crashed, externalEdited>>

BlockExternalOrCasMismatch ==
    /\ phase = "verified"
    /\ (obs = "external" \/ (obs = "post" /\ target # "post") \/ (obs = "bad" /\ target # "bad"))
    /\ journal' = "unknown"
    /\ phase' = "unknown"
    /\ lock' = 0
    /\ UNCHANGED <<dry, backup, target, obs, receipt, crashed, externalEdited>>

FinishUnchangedPre ==
    /\ phase = "verified"
    /\ obs = "pre"
    /\ journal' = "failed"
    /\ phase' = "failed"
    /\ lock' = 0
    /\ UNCHANGED <<dry, backup, target, obs, receipt, crashed, externalEdited>>

Crash ==
    /\ ~crashed
    /\ phase \in {"locked", "prepared", "mutating", "written", "verified", "committed"}
    /\ phase' = (IF journal = "none" THEN "failed" ELSE "interrupted")
    /\ lock' = 0
    /\ crashed' = TRUE
    /\ UNCHANGED <<dry, journal, backup, target, obs, receipt, externalEdited>>

ExternalEdit ==
    /\ ~externalEdited
    /\ phase \in {"prepared", "mutating", "written", "verified", "interrupted"}
    /\ receipt # "post"
    /\ target' = "external"
    /\ externalEdited' = TRUE
    /\ UNCHANGED <<phase, dry, lock, journal, backup, obs, receipt, crashed>>

RecoverAcquire ==
    /\ phase \in {"interrupted", "unknown"}
    /\ lock = 0
    /\ lock' = 1
    /\ phase' = "recovering"
    /\ UNCHANGED <<dry, journal, backup, target, obs, receipt, crashed, externalEdited>>

RecoverPrepared ==
    /\ phase = "recovering"
    /\ journal = "prepared"
    /\ target = "pre"
    /\ journal' = "failed"
    /\ phase' = "failed"
    /\ lock' = 0
    /\ UNCHANGED <<dry, backup, target, obs, receipt, crashed, externalEdited>>

RecoverCommitted ==
    /\ phase = "recovering"
    /\ receipt = "post"
    /\ target = "post"
    /\ journal' = "succeeded"
    /\ phase' = "succeeded"
    /\ lock' = 0
    /\ UNCHANGED <<dry, backup, target, obs, receipt, crashed, externalEdited>>

RecoverCheck ==
    /\ phase = "recovering"
    /\ ~(journal = "prepared" /\ target = "pre")
    /\ ~(receipt = "post" /\ target = "post")
    /\ phase' = "recovering_check"
    /\ UNCHANGED <<dry, lock, journal, backup, target, obs, receipt, crashed, externalEdited>>

OtherAcquire ==
    /\ lock = 0
    /\ journal \notin {"prepared", "mutating", "unknown"}
    /\ lock' = 2
    /\ UNCHANGED <<phase, dry, journal, backup, target, obs, receipt, crashed, externalEdited>>

OtherRelease ==
    /\ lock = 2
    /\ lock' = 0
    /\ UNCHANGED <<phase, dry, journal, backup, target, obs, receipt, crashed, externalEdited>>

Terminal == phase \in {"previewed", "noop", "succeeded", "failed", "unknown"}
Done == /\ Terminal /\ lock = 0 /\ UNCHANGED vars

Progress ==
    \/ Preview
    \/ Noop
    \/ Acquire
    \/ SaveBackupAndPrepare
    \/ BeginMutate
    \/ WriteTargetCasOk
    \/ AbortBeforeWriteOnCasMismatch
    \/ PostVerify
    \/ CommitReceipt
    \/ FinishSuccess
    \/ RollbackOnBadCasOk
    \/ BlockExternalOrCasMismatch
    \/ FinishUnchangedPre
    \/ RecoverAcquire
    \/ RecoverPrepared
    \/ RecoverCommitted
    \/ RecoverCheck

Next ==
    \/ Progress
    \/ Crash
    \/ ExternalEdit
    \/ OtherAcquire
    \/ OtherRelease
    \/ Done

Spec == Init /\ [][Next]_vars /\ SF_vars(Progress) /\ WF_vars(OtherRelease)

PreviewDoesNotWrite ==
    dry => /\ journal = "none" /\ backup = "none"

NoopDoesNotWrite ==
    phase = "noop" => /\ journal = "none" /\ backup = "none" /\ target = "post" /\ receipt = "post"

BackupBeforeMutation ==
    (journal \in {"prepared", "mutating", "unknown", "restored", "succeeded"} \/ target = "bad") => backup = "saved"

NoOverwriteExternalEdit ==
    externalEdited => target = "external"

VerifiedSuccess ==
    phase = "succeeded" => /\ target = "post" /\ receipt = "post" /\ backup = "saved" /\ ~externalEdited

PendingBlocksOther ==
    journal \in {"prepared", "mutating", "unknown"} => lock # 2

ExclusiveExecution ==
    phase \in {"locked", "prepared", "mutating", "written", "verified", "committed", "recovering", "recovering_check"} => lock = 1

EventuallyTerminal == <>Terminal

NoApplySuccess == ~(phase = "succeeded" /\ ~crashed)
NoRollbackRecovery == ~(journal = "restored" /\ target = "pre")
NoExternalConflictUnknown == ~(phase = "unknown" /\ externalEdited /\ ~crashed)
NoCrashAfterCommitRecovery == ~(phase = "succeeded" /\ crashed)
=============================================================================
