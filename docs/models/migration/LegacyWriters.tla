-------------------------- MODULE LegacyWriters --------------------------
VARIABLES phase, evidence, frozen, atPlacement, atCommit, placed, authority
vars == <<phase, evidence, frozen, atPlacement, atCommit, placed, authority>>

Valid == {"absent", "masked", "replacement_mask"}
Observations == Valid \cup {"live_restored", "source_restored"}

Init ==
    /\ phase = "preflight"
    /\ evidence \in Observations
    /\ frozen = "unseen"
    /\ atPlacement = "unseen"
    /\ atCommit = "unseen"
    /\ placed = FALSE
    /\ authority = "old"

Preview ==
    /\ phase = "preflight"
    /\ phase' = "before_lock"
    /\ UNCHANGED <<evidence, frozen, atPlacement, atCommit, placed, authority>>

ObserveChange ==
    /\ phase \in {"before_lock", "prepared", "placed", "published", "cleanup_pending"}
    /\ evidence' \in Observations
    /\ UNCHANGED <<phase, frozen, atPlacement, atCommit, placed, authority>>

Prepare ==
    /\ phase = "before_lock"
    /\ phase' = IF evidence \in Valid THEN "prepared" ELSE "blocked"
    /\ frozen' = IF evidence \in Valid THEN evidence ELSE "unseen"
    /\ UNCHANGED <<evidence, atPlacement, atCommit, placed, authority>>

Place ==
    /\ phase = "prepared"
    /\ evidence = frozen
    /\ phase' = "placed"
    /\ placed' = TRUE
    /\ atPlacement' = evidence
    /\ UNCHANGED <<evidence, frozen, atCommit, authority>>

Publish ==
    /\ phase = "placed"
    /\ evidence = frozen
    /\ phase' = "published"
    /\ authority' = "new"
    /\ atCommit' = evidence
    /\ UNCHANGED <<evidence, frozen, atPlacement, placed>>

Rollback ==
    /\ phase \in {"prepared", "placed"}
    /\ evidence # frozen
    /\ phase' = "rolled_back"
    /\ placed' = FALSE
    /\ UNCHANGED <<evidence, frozen, atPlacement, atCommit, authority>>

Finish ==
    /\ phase \in {"published", "cleanup_pending"}
    /\ phase' = IF evidence \in Valid THEN "settled" ELSE "cleanup_pending"
    /\ UNCHANGED <<evidence, frozen, atPlacement, atCommit, placed, authority>>

EnableNew ==
    /\ phase = "settled"
    /\ phase' = "new_enabled"
    /\ UNCHANGED <<evidence, frozen, atPlacement, atCommit, placed, authority>>

Terminal == phase \in {"blocked", "rolled_back", "cleanup_pending", "new_enabled"}
Done == /\ Terminal /\ UNCHANGED vars

Progress == Preview \/ Prepare \/ Place \/ Publish \/ Rollback \/ Finish \/ EnableNew
Next == Progress \/ ObserveChange \/ Done
Spec == Init /\ [][Next]_vars /\ SF_vars(Progress)

FrozenBeforePlace == placed => atPlacement = frozen /\ frozen \in Valid
FreshBeforeCommit == authority = "new" => placed /\ atCommit = frozen /\ atCommit \in Valid
PreCommitFailureLeavesOld == phase \in {"blocked", "rolled_back"} => authority = "old" /\ ~placed
PostCommitKeepsNew == phase \in {"published", "cleanup_pending", "settled", "new_enabled"} => authority = "new"
EventuallyOutcome == <>Terminal

NoRestoredSourceBeforeRun == ~(phase = "blocked" /\ evidence = "source_restored")
NoRestoredEntrypointBeforePlace == ~(phase = "rolled_back" /\ atPlacement = "unseen" /\ evidence = "live_restored")
NoIdentityDriftBeforeCommit == ~(phase = "rolled_back" /\ atPlacement = "masked" /\ evidence = "replacement_mask")
NoPendingCleanup == ~(phase = "cleanup_pending" /\ authority = "new")
NoEnableNew == ~(phase = "new_enabled" /\ authority = "new")
=============================================================================
