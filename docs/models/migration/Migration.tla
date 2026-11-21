--------------------------- MODULE Migration ---------------------------
EXTENDS Naturals, TLC

VARIABLES phase, dry, oldPending, targetConflict, lock, journal, oldData, staging, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked
vars == <<phase, dry, oldPending, targetConflict, lock, journal, oldData, staging, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

AuthoritativeRoot ==
    IF ledger = "committed" THEN "new" ELSE "old"

Init ==
    /\ phase = "planned"
    /\ dry \in BOOLEAN
    /\ oldPending \in BOOLEAN
    /\ targetConflict \in BOOLEAN
    /\ lock = 0
    /\ journal = "none"
    /\ oldData = "intact"
    /\ staging = "empty"
    /\ placed = "none"
    /\ newAuthority = "absent"
    /\ ledger = "none"
    /\ crashed = FALSE
    /\ postCommitUpdated = FALSE
    /\ legacyWroteAfterCutover = FALSE
    /\ cleanVerifySeen = FALSE
    /\ rerunChecked = FALSE

PreflightOnly ==
    /\ phase = "planned"
    /\ dry
    /\ phase' = "preflight_done"
    /\ UNCHANGED <<dry, oldPending, targetConflict, lock, journal, oldData, staging, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

BlockPreflightGate ==
    /\ phase = "planned"
    /\ ~dry
    /\ (oldPending \/ targetConflict)
    /\ phase' = "blocked_preflight"
    /\ UNCHANGED <<dry, oldPending, targetConflict, lock, journal, oldData, staging, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

AcquireLocks ==
    /\ phase = "planned"
    /\ ~dry
    /\ ~oldPending
    /\ ~targetConflict
    /\ lock = 0
    /\ journal = "none"
    /\ lock' = 1
    /\ journal' = "prepared"
    /\ phase' = "locked"
    /\ UNCHANGED <<dry, oldPending, targetConflict, oldData, staging, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

StageWithPlacementManifest ==
    /\ phase = "locked"
    /\ ledger = "none"
    /\ staging' \in {"staged_with_manifest", "bad_digest"}
    /\ phase' = "staged"
    /\ UNCHANGED <<dry, oldPending, targetConflict, lock, journal, oldData, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

VerifyStagingOk ==
    /\ phase = "staged"
    /\ staging = "staged_with_manifest"
    /\ journal' = "placing"
    /\ phase' = "verified"
    /\ UNCHANGED <<dry, oldPending, targetConflict, lock, oldData, staging, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

VerifyStagingFail ==
    /\ phase = "staged"
    /\ staging = "bad_digest"
    /\ staging' = "cleaned"
    /\ journal' = "failed"
    /\ phase' = "failed"
    /\ lock' = 0
    /\ UNCHANGED <<dry, oldPending, targetConflict, oldData, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

PlaceTargetsStep ==
    /\ phase = "verified"
    /\ placed = "none"
    /\ placed' \in {"partial", "all"}
    /\ newAuthority' = (IF placed' = "all" THEN "ready" ELSE "absent")
    /\ phase' = "placed_step"
    /\ UNCHANGED <<dry, oldPending, targetConflict, lock, journal, oldData, staging, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

FinishPartialPlacement ==
    /\ phase = "placed_step"
    /\ placed = "partial"
    /\ placed' = "all"
    /\ newAuthority' = "ready"
    /\ UNCHANGED <<phase, dry, oldPending, targetConflict, lock, journal, oldData, staging, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

CommitMigrationLedger ==
    /\ phase = "placed_step"
    /\ placed = "all"
    /\ newAuthority = "ready"
    /\ ledger' = "committed"
    /\ staging' = "cleaned"
    /\ journal' = "succeeded"
    /\ phase' = "succeeded"
    /\ lock' = 0
    /\ UNCHANGED <<dry, oldPending, targetConflict, oldData, placed, newAuthority, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

PostSwitchBusinessWrite ==
    /\ phase = "succeeded"
    /\ ledger = "committed"
    /\ ~postCommitUpdated
    /\ newAuthority' = "post_switch_updated"
    /\ postCommitUpdated' = TRUE
    /\ UNCHANGED <<phase, dry, oldPending, targetConflict, lock, journal, oldData, staging, placed, ledger, crashed, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

LegacyWriteAfterCutover ==
    /\ phase = "succeeded"
    /\ ledger = "committed"
    /\ ~legacyWroteAfterCutover
    /\ oldData' = "diverged_after_cutover"
    /\ legacyWroteAfterCutover' = TRUE
    /\ rerunChecked' = FALSE
    /\ UNCHANGED <<phase, dry, oldPending, targetConflict, lock, journal, staging, placed, newAuthority, ledger, crashed, postCommitUpdated, cleanVerifySeen>>

VerifyOrRerunClean ==
    /\ phase = "succeeded"
    /\ ledger = "committed"
    /\ oldData = "intact"
    /\ ~rerunChecked
    /\ lock = 0
    /\ cleanVerifySeen' = TRUE
    /\ rerunChecked' = TRUE
    /\ UNCHANGED <<phase, dry, oldPending, targetConflict, lock, journal, oldData, staging, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover>>

VerifyOrRerunDetectsDivergence ==
    /\ phase = "succeeded"
    /\ ledger = "committed"
    /\ oldData = "diverged_after_cutover"
    /\ ~rerunChecked
    /\ lock = 0
    /\ rerunChecked' = TRUE
    /\ phase' = "blocked_divergence"
    /\ UNCHANGED <<dry, oldPending, targetConflict, lock, journal, oldData, staging, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen>>

Crash ==
    /\ ~crashed
    /\ phase \in {"locked", "staged", "verified", "placed_step"}
    /\ phase' = "interrupted"
    /\ lock' = 0
    /\ crashed' = TRUE
    /\ UNCHANGED <<dry, oldPending, targetConflict, journal, oldData, staging, placed, newAuthority, ledger, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

RecoverAcquire ==
    /\ phase = "interrupted"
    /\ lock = 0
    /\ lock' = 1
    /\ phase' = "recovering"
    /\ UNCHANGED <<dry, oldPending, targetConflict, journal, oldData, staging, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

RecoverPreCommitRollbackByManifest ==
    /\ phase = "recovering"
    /\ ledger = "none"
    /\ placed' = "none"
    /\ staging' = "cleaned"
    /\ newAuthority' = "absent"
    /\ journal' = "rolled_back"
    /\ phase' = "rolled_back"
    /\ lock' = 0
    /\ UNCHANGED <<dry, oldPending, targetConflict, oldData, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

OtherAcquire ==
    /\ lock = 0
    /\ journal \notin {"prepared", "placing"}
    /\ lock' = 2
    /\ UNCHANGED <<phase, dry, oldPending, targetConflict, journal, oldData, staging, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

OtherRelease ==
    /\ lock = 2
    /\ lock' = 0
    /\ UNCHANGED <<phase, dry, oldPending, targetConflict, journal, oldData, staging, placed, newAuthority, ledger, crashed, postCommitUpdated, legacyWroteAfterCutover, cleanVerifySeen, rerunChecked>>

Terminal == phase \in {"preflight_done", "blocked_preflight", "succeeded", "failed", "rolled_back", "blocked_divergence"}
Done == /\ Terminal /\ lock = 0 /\ UNCHANGED vars

Progress ==
    \/ PreflightOnly
    \/ BlockPreflightGate
    \/ AcquireLocks
    \/ StageWithPlacementManifest
    \/ VerifyStagingOk
    \/ VerifyStagingFail
    \/ PlaceTargetsStep
    \/ FinishPartialPlacement
    \/ CommitMigrationLedger
    \/ RecoverAcquire
    \/ RecoverPreCommitRollbackByManifest

Next ==
    \/ Progress
    \/ PostSwitchBusinessWrite
    \/ LegacyWriteAfterCutover
    \/ VerifyOrRerunClean
    \/ VerifyOrRerunDetectsDivergence
    \/ Crash
    \/ OtherAcquire
    \/ OtherRelease
    \/ Done

Spec == Init /\ [][Next]_vars /\ SF_vars(Progress) /\ WF_vars(OtherRelease)

PreflightZeroWrites ==
    dry => /\ staging = "empty" /\ placed = "none" /\ newAuthority = "absent" /\ ledger = "none" /\ journal = "none"

PreflightGateBlocks ==
    (oldPending \/ targetConflict) => /\ staging = "empty" /\ placed = "none" /\ newAuthority = "absent" /\ ledger = "none"

ConverterNeverMutatesOldSource ==
    ~legacyWroteAfterCutover => oldData = "intact"

AuthoritativeVisibilityClean ==
    (AuthoritativeRoot = "old") <=> (ledger = "none")

LedgerImpliesAllPlacedAndVerified ==
    ledger = "committed" => /\ placed = "all" /\ newAuthority \in {"ready", "post_switch_updated"}

PostCommitNeverClobbersNewBusinessState ==
    postCommitUpdated => /\ newAuthority = "post_switch_updated" /\ ledger = "committed"

DivergenceDetectedWithoutClobber ==
    phase = "blocked_divergence" =>
        /\ legacyWroteAfterCutover
        /\ oldData = "diverged_after_cutover"
        /\ ledger = "committed"
        /\ newAuthority \in {"ready", "post_switch_updated"}

DivergenceNeverPassesVerify ==
    (rerunChecked /\ oldData = "diverged_after_cutover") => phase = "blocked_divergence"

PreCommitRollbackCleansManifestItems ==
    phase \in {"failed", "rolled_back"} =>
        /\ ledger = "none"
        /\ placed = "none"
        /\ newAuthority = "absent"
        /\ oldData = "intact"

InFlightBlocksOther ==
    journal \in {"prepared", "placing"} => lock # 2

ExclusiveMigration ==
    phase \in {"locked", "staged", "verified", "placed_step", "recovering"} => lock = 1

EventuallyTerminal == <>Terminal

NoMigrationSuccess == ~(phase = "succeeded" /\ ledger = "committed")
NoPartialPlacementCrashRollback == ~(phase = "rolled_back" /\ crashed)
NoPostSwitchPreserved == ~(postCommitUpdated /\ rerunChecked /\ newAuthority = "post_switch_updated")
NoDivergenceBlocked == ~(phase = "blocked_divergence" /\ postCommitUpdated)
NoLateDivergenceAfterCleanVerify == ~(phase = "blocked_divergence" /\ cleanVerifySeen /\ postCommitUpdated)
=============================================================================
