-------------------------- MODULE CapabilityTx --------------------------
EXTENDS Naturals, TLC

VARIABLE s
vars == <<s>>
Modes == {"sync", "upgrade_overlaid", "policy_recover"}
Keys == {"native", "managed", "overlay", "receipt"}
PreFiles(m) == IF m = "sync" THEN "v1_raw" ELSE "v1_applied"
PostFiles(m) == IF m = "sync" THEN "v1_applied" ELSE "v2_applied"
PreLedger(m) == IF m = "sync" THEN "old" ELSE "v1_committed"
PostLedger(m) == IF m = "sync" THEN "v1_committed" ELSE "v2_committed"
BeforeLedgers == [k \in Keys |-> "pre"]
AfterValue(m,k) == IF m = "sync" /\ k \in {"native", "managed"} THEN "pre" ELSE "post"
AfterLedgers(m) == [k \in Keys |-> AfterValue(m,k)]
AllAfter == s.ledgers = AfterLedgers(s.mode)
KnownLedgers == \A k \in Keys : s.ledgers[k] \in {"pre",AfterValue(s.mode,k)}
OwnFiles == s.files \in {PreFiles(s.mode),PostFiles(s.mode),"partial"}
HasExternalLedger == \E k \in Keys : s.ledgers[k] = "external"
CommitPhases == {"verified", "commit_managed", "commit_overlay", "commit_receipt", "clear_pending"}
RestorePhases == {"restore_receipt", "restore_overlay", "restore_managed", "restore_native", "restore_payload", "rollback_done"}
ActivePhases == {"prepared", "mutating", "written", "interrupted", "recovering", "recovering_verify"} \cup CommitPhases \cup RestorePhases

Init == \E d \in BOOLEAN, m \in Modes, current \in BOOLEAN :
    /\ (current => m = "sync")
    /\ s = [phase |-> "planned", dry |-> d, mode |-> m, lock |-> 0,
             journal |-> "none", backupSaved |-> FALSE, pendingOverlay |-> "none",
             files |-> IF current THEN PostFiles(m) ELSE PreFiles(m),
             overlayLedger |-> IF current THEN PostLedger(m) ELSE PreLedger(m),
             ledgers |-> IF current THEN AfterLedgers(m) ELSE BeforeLedgers,
             initialFiles |-> IF current THEN PostFiles(m) ELSE PreFiles(m),
             initialOverlay |-> IF current THEN PostLedger(m) ELSE PreLedger(m),
             initialLedgers |-> IF current THEN AfterLedgers(m) ELSE BeforeLedgers,
             alreadyCurrent |-> current, crashed |-> FALSE, externalTamper |-> FALSE,
             externalLedger |-> FALSE, control |-> "pre", sawNativePartialCrash |-> FALSE,
             sawOverlayPartialCrash |-> FALSE, sawRollbackCrash |-> FALSE]

Preview == /\ s.phase = "planned" /\ s.dry /\ s' = [s EXCEPT !.phase = "previewed"]
PolicyRecoverNoop ==
    /\ s.phase = "planned" /\ ~s.dry /\ s.mode = "policy_recover"
    /\ s.pendingOverlay = "none" /\ s' = [s EXCEPT !.phase = "not_needed"]
AlreadyCurrentNoop ==
    /\ s.phase = "planned" /\ ~s.dry /\ s.alreadyCurrent
    /\ s' = [s EXCEPT !.phase = "not_needed"]
Acquire ==
    /\ s.phase = "planned" /\ ~s.dry /\ ~s.alreadyCurrent
    /\ s.mode \in {"sync", "upgrade_overlaid"} /\ s.lock = 0 /\ s.journal = "none"
    /\ s.pendingOverlay = "none" /\ s' = [s EXCEPT !.lock = 1, !.phase = "locked"]
StageBackupAndPending ==
    /\ s.phase = "locked" /\ s.files = PreFiles(s.mode)
    /\ s' = [s EXCEPT !.backupSaved = TRUE, !.pendingOverlay = "staged", !.journal = "prepared", !.phase = "prepared"]
BeginMutate ==
    /\ s.phase = "prepared" /\ s.backupSaved /\ s.pendingOverlay = "staged"
    /\ s.control = "pre" /\ s.files = PreFiles(s.mode) /\ KnownLedgers
    /\ s' = [s EXCEPT !.journal = "mutating", !.phase = "mutating"]
ApplyChangesCasOk ==
    /\ s.phase = "mutating" /\ s.files = PreFiles(s.mode) /\ s.control = "pre" /\ KnownLedgers
    /\ \E result \in {PostFiles(s.mode),"partial"} : s' = [s EXCEPT !.files = result, !.phase = "written"]
VerifyBatch ==
    /\ (s.phase = "written" \/ (s.phase = "recovering_verify" /\ AllAfter))
    /\ s.files = PostFiles(s.mode) /\ s.control = "pre" /\ KnownLedgers
    /\ s' = [s EXCEPT !.phase = IF AllAfter THEN "clear_pending" ELSE "verified"]

CommitPhysical(k,from,to) ==
    /\ s.phase = from /\ s.files = PostFiles(s.mode) /\ s.control = "pre"
    /\ KnownLedgers /\ s.ledgers[k] = "pre"
    /\ s' = [s EXCEPT !.ledgers[k] = AfterValue(s.mode,k), !.phase = to]
CommitNative == CommitPhysical("native","verified","commit_managed")
CommitManaged == CommitPhysical("managed","commit_managed","commit_overlay")
CommitOverlay == CommitPhysical("overlay","commit_overlay","commit_receipt")
CommitReceipts == CommitPhysical("receipt","commit_receipt","clear_pending")
ClearPending ==
    /\ s.phase = "clear_pending" /\ s.files = PostFiles(s.mode) /\ AllAfter /\ s.control = "pre"
    /\ s' = [s EXCEPT !.overlayLedger = PostLedger(s.mode), !.pendingOverlay = "cleared", !.phase = "committed"]
Finish ==
    /\ s.phase = "committed"
    /\ s' = [s EXCEPT !.journal = "succeeded", !.phase = "succeeded", !.lock = 0]

BeginRollback ==
    /\ s.phase \in {"prepared","mutating","written","recovering_verify"} \cup CommitPhases
    /\ OwnFiles /\ KnownLedgers /\ s.pendingOverlay = "staged"
    /\ (s.control = "external" \/ s.files \in {PreFiles(s.mode),"partial"}
        \/ (s.phase = "recovering_verify" /\ ~AllAfter))
    /\ s' = [s EXCEPT !.phase = "restore_receipt"]
RestorePhysical(k,from,to) ==
    /\ s.phase = from /\ OwnFiles /\ KnownLedgers
    /\ s' = [s EXCEPT !.ledgers[k] = "pre", !.phase = to]
RestoreReceipts == RestorePhysical("receipt","restore_receipt","restore_overlay")
RestoreOverlay == RestorePhysical("overlay","restore_overlay","restore_managed")
RestoreManaged == RestorePhysical("managed","restore_managed","restore_native")
RestoreNative == RestorePhysical("native","restore_native","restore_payload")
RestorePayload ==
    /\ s.phase = "restore_payload" /\ OwnFiles /\ s.ledgers = BeforeLedgers
    /\ s' = [s EXCEPT !.files = PreFiles(s.mode), !.phase = "rollback_done"]
ClearRollbackPending ==
    /\ s.phase = "rollback_done" /\ s.files = PreFiles(s.mode) /\ s.ledgers = BeforeLedgers
    /\ s' = [s EXCEPT !.pendingOverlay = "cleared", !.overlayLedger = PreLedger(s.mode), !.phase = "rolled_back"]
FinishRollback ==
    /\ s.phase = "rolled_back"
    /\ s' = [s EXCEPT !.journal = "restored", !.phase = "failed", !.lock = 0]
BlockTampered ==
    /\ s.phase \in ActivePhases /\ s.lock = 1
    /\ (s.files = "external" \/ HasExternalLedger)
    /\ s' = [s EXCEPT !.journal = "unknown", !.phase = "unknown", !.lock = 0]

Crash ==
    /\ ~s.crashed
    /\ s.phase \in {"locked","prepared","mutating","written","committed","rolled_back"} \cup CommitPhases \cup RestorePhases
    /\ s' = [s EXCEPT !.phase = IF s.journal = "none" THEN "failed" ELSE "interrupted",
                     !.lock = 0, !.crashed = TRUE,
                     !.sawNativePartialCrash = (s.ledgers.native = "post" /\ s.ledgers.managed = "pre"),
                     !.sawOverlayPartialCrash = (s.ledgers.overlay = "post" /\ s.ledgers.receipt = "pre"),
                     !.sawRollbackCrash = (s.phase \in RestorePhases)]
ExternalModify ==
    /\ ~s.externalTamper /\ s.phase \in ActivePhases /\ s.pendingOverlay = "staged"
    /\ s' = [s EXCEPT !.files = "external", !.externalTamper = TRUE]
ExternalLedgerModify ==
    /\ ~s.externalLedger /\ s.phase \in ActivePhases /\ s.pendingOverlay = "staged"
    /\ \E k \in Keys : s' = [s EXCEPT !.ledgers[k] = "external", !.externalLedger = TRUE]
ExternalInputModify ==
    /\ s.control = "pre" /\ s.phase \in ActivePhases /\ s.pendingOverlay = "staged"
    /\ s' = [s EXCEPT !.control = "external"]
RecoverAcquire ==
    /\ s.phase \in {"interrupted","unknown"} /\ s.lock = 0
    /\ s' = [s EXCEPT !.lock = 1, !.phase = "recovering"]
RecoverAlreadyCommitted ==
    /\ s.phase = "recovering" /\ s.pendingOverlay = "cleared" /\ AllAfter
    /\ s.overlayLedger = PostLedger(s.mode) /\ s.files = PostFiles(s.mode)
    /\ s' = [s EXCEPT !.journal = "succeeded", !.phase = "succeeded", !.lock = 0]
RecoverVerify ==
    /\ s.phase = "recovering" /\ s.pendingOverlay = "staged"
    /\ s' = [s EXCEPT !.phase = "recovering_verify"]
RecoverRolledBack ==
    /\ s.phase = "recovering" /\ s.pendingOverlay = "cleared" /\ s.ledgers = BeforeLedgers
    /\ s.overlayLedger = PreLedger(s.mode) /\ s.files = PreFiles(s.mode)
    /\ s' = [s EXCEPT !.journal = "restored", !.phase = "failed", !.lock = 0]

OtherAcquire ==
    /\ s.lock = 0 /\ s.pendingOverlay # "staged" /\ s.journal \notin {"prepared","mutating","unknown"}
    /\ s' = [s EXCEPT !.lock = 2]
OtherRelease == /\ s.lock = 2 /\ s' = [s EXCEPT !.lock = 0]
Terminal == s.phase \in {"previewed","not_needed","succeeded","failed","unknown"}
Done == /\ Terminal /\ s.lock = 0 /\ UNCHANGED vars
Progress ==
    \/ Preview \/ PolicyRecoverNoop \/ AlreadyCurrentNoop \/ Acquire \/ StageBackupAndPending
    \/ BeginMutate \/ ApplyChangesCasOk \/ VerifyBatch \/ CommitNative \/ CommitManaged
    \/ CommitOverlay \/ CommitReceipts \/ ClearPending \/ Finish \/ BeginRollback
    \/ RestoreReceipts \/ RestoreOverlay \/ RestoreManaged \/ RestoreNative \/ RestorePayload
    \/ ClearRollbackPending \/ FinishRollback \/ BlockTampered \/ RecoverAcquire \/ RecoverAlreadyCommitted \/ RecoverVerify \/ RecoverRolledBack
Next == Progress \/ Crash \/ ExternalModify \/ ExternalLedgerModify \/ ExternalInputModify \/ OtherAcquire \/ OtherRelease \/ Done
Spec == Init /\ [][Next]_vars /\ SF_vars(Progress) /\ WF_vars(OtherRelease)

PreviewDoesNotWrite == s.dry => /\ ~s.backupSaved /\ s.pendingOverlay = "none" /\ s.journal = "none" /\ s.ledgers = s.initialLedgers
PolicyRecoverWithoutPendingIsNoop ==
    s.mode = "policy_recover" => /\ s.files = "v1_applied" /\ s.overlayLedger = "v1_committed"
                                /\ ~s.backupSaved /\ s.pendingOverlay = "none" /\ s.journal = "none"
BackupAndPendingBeforeMutation == (s.journal \in {"prepared","mutating","unknown","restored","succeeded"} \/ s.files = "partial") => s.backupSaved
NoOverwriteExternalTamper == s.externalTamper => s.files = "external"
VerifiedSyncOrUpgrade == s.phase = "succeeded" => /\ s.mode \in {"sync","upgrade_overlaid"}
    /\ s.pendingOverlay = "cleared" /\ s.files = PostFiles(s.mode) /\ s.overlayLedger = PostLedger(s.mode)
    /\ AllAfter /\ ~s.externalTamper /\ s.control = "pre"
NoStaleOverlayAfterUpgrade ==
    /\ (s.overlayLedger = "v2_committed" => s.files = "v2_applied")
    /\ ((s.mode = "upgrade_overlaid" /\ s.phase = "failed" /\ ~s.externalTamper) =>
        (s.files = "v1_applied" /\ s.overlayLedger = "v1_committed"))
PendingBlocksOther == (s.pendingOverlay = "staged" \/ s.journal \in {"prepared","mutating","unknown"}) => s.lock # 2
ExclusiveExecution == s.phase \in (({"locked","committed","rolled_back"} \cup ActivePhases) \ {"interrupted"}) => s.lock = 1
NoOverwriteExternalLedger == s.externalLedger => HasExternalLedger
AllLedgersBeforeClearing == (s.pendingOverlay = "cleared" /\ s.overlayLedger = PostLedger(s.mode)) => AllAfter
AlreadyCurrentDoesNotWrite == s.alreadyCurrent => /\ s.files = s.initialFiles /\ s.ledgers = s.initialLedgers /\ ~s.backupSaved /\ s.journal = "none"
EventuallyTerminal == <>Terminal

NoSyncSuccess == ~(s.phase = "succeeded" /\ s.mode = "sync")
NoPolicyRecoverNoop == ~(s.phase = "not_needed" /\ s.mode = "policy_recover")
NoInterruptedPendingRecovery == ~(s.crashed /\ s.journal = "restored" /\ s.pendingOverlay = "cleared" /\ s.files = PreFiles(s.mode))
NoUpgradeOverlayRebaseSuccess == ~(s.phase = "succeeded" /\ s.mode = "upgrade_overlaid")
NoTamperUnknown == ~(s.phase = "unknown" /\ s.externalTamper /\ ~s.crashed)
NoNativePartialRecovery == ~(s.phase = "failed" /\ s.sawNativePartialCrash /\ s.ledgers = BeforeLedgers)
NoOverlayPartialRecovery == ~(s.phase = "failed" /\ s.sawOverlayPartialCrash /\ s.ledgers = BeforeLedgers)
NoLedgerTamperBlocked == ~(s.phase = "unknown" /\ s.externalLedger)
NoRollbackCrashRecovery == ~(s.phase = "failed" /\ s.sawRollbackCrash)
NoSyncedNoop == ~(s.phase = "not_needed" /\ s.mode = "sync")
=============================================================================
