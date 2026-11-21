----------------------------- MODULE ChezmoiSource -----------------------------
EXTENDS Naturals

CONSTANT Mode
ASSUME Mode \in {"add", "forget"}
VARIABLE s
vars == <<s>>
Before == IF Mode = "add" THEN "missing" ELSE "original"
Desired == IF Mode = "add" THEN "candidate" ELSE "missing"
LiveReady == s.canonicalState /\ s.owner /\ s.inputs /\ s.target /\ s.source = Before
HoldingReady == IF Mode = "add" THEN s.holding = "empty" /\ s.candidate = "consumed" ELSE s.holding = "original"
After == s.canonicalState /\ s.owner /\ s.inputs /\ s.target /\ s.source = Desired /\ HoldingReady
Init ==
    s = [phase |-> "preview", canonicalState |-> TRUE, owner |-> TRUE, inputs |-> TRUE,
         target |-> TRUE, source |-> Before, holding |-> "empty", candidate |-> "not-staged", native |-> "none",
         nativeRuns |-> 0, attempts |-> 0, canonicalAtWrite |-> FALSE, backed |-> FALSE, synced |-> FALSE, cache |-> "old", marker |-> FALSE]
Stage ==
    /\ s.phase = "preview" /\ LiveReady
    /\ \/ s' = [s EXCEPT !.phase = "staged", !.candidate = "owned", !.native = "private", !.nativeRuns = @ + 1]
       \/ s' = [s EXCEPT !.phase = "cancelled", !.native = "private", !.nativeRuns = @ + 1]
Confirm == s.phase = "staged" /\ LiveReady /\ s' = [s EXCEPT !.phase = "confirmed"]
Prepare == s.phase = "confirmed" /\ LiveReady /\ s' = [s EXCEPT !.phase = "prepared"]
Guard == s.phase = "prepared" /\ LiveReady /\ s' = [s EXCEPT !.phase = "guarded"]
Drift ==
    /\ s.phase \in {"preview", "staged", "confirmed", "prepared", "guarded", "moved", "unknown", "post"}
    /\ \/ s' = [s EXCEPT !.canonicalState = FALSE]
       \/ s' = [s EXCEPT !.owner = FALSE]
       \/ s' = [s EXCEPT !.inputs = FALSE]
       \/ s' = [s EXCEPT !.target = FALSE]
       \/ s' = [s EXCEPT !.source = "other"]
       \/ s.attempts = 1 /\ Mode = "add" /\ s' = [s EXCEPT !.source = "missing"]
       \/ s.candidate = "owned" /\ s' = [s EXCEPT !.candidate = "other"]
       \/ Mode = "forget" /\ s.holding = "original"
          /\ s' = [s EXCEPT !.holding = "other"]
AddSlot ==
    /\ Mode = "add" /\ s.phase = "guarded" /\ s.canonicalState
    /\ s.source = Before /\ s.candidate = "owned"
    /\ s' = [s EXCEPT !.source = Desired, !.candidate = "consumed", !.phase = "moved", !.attempts = @ + 1, !.canonicalAtWrite = s.canonicalState,
                  !.backed = TRUE]
RetireSlot ==
    /\ Mode = "forget" /\ s.phase = "guarded" /\ s.canonicalState /\ s.source \in {"original", "other"}
    /\ s.holding = "empty"
    /\ s' = [s EXCEPT !.source = "missing", !.holding = s.source, !.phase = "moved",
                  !.attempts = @ + 1, !.canonicalAtWrite = s.canonicalState, !.backed = s.source = "original"]
RestoreUnexpected ==
    /\ s.phase \in {"moved", "unknown"} /\ s.holding = "other" /\ s.source = "missing"
    /\ s.canonicalState /\ s.owner /\ s.cache = "old"
    /\ s' = [s EXCEPT !.source = "other", !.holding = "empty", !.phase = "unknown"]
SyncSource ==
    /\ s.phase \in {"moved", "unknown"} /\ s.backed /\ After
    /\ s' = [s EXCEPT !.synced = TRUE]
Publish ==
    /\ s.phase \in {"moved", "unknown"} /\ s.backed /\ After /\ s.synced
    /\ s' = [s EXCEPT !.cache = "updated", !.marker = s.backed /\ After /\ s.synced, !.phase = "post"]
Hold ==
    /\ s.phase \in {"guarded", "moved"} /\ ~(s.backed /\ After)
    /\ s' = [s EXCEPT !.phase = "unknown"]
CloseOld ==
    /\ s.phase \in {"prepared", "guarded", "unknown"}
    /\ IF Mode = "add" THEN s.candidate = "owned" ELSE ~s.backed
    /\ LiveReady /\ s.holding = "empty" /\ s.cache = "old"
    /\ s' = [s EXCEPT !.phase = "not-committed"]
Finish ==
    /\ s.phase = "post" /\ s.marker
    /\ s' = [s EXCEPT !.phase = "succeeded"]
Next ==
    Stage \/ Confirm \/ Prepare \/ Guard \/ Drift \/ AddSlot \/ RetireSlot
    \/ RestoreUnexpected \/ SyncSource \/ Publish \/ Hold \/ CloseOld \/ Finish

OnlyPrivateNative == s.native \in {"none", "private"} /\ s.nativeRuns \in 0..1
OnlyOneSourceWrite == s.attempts \in 0..1
AddDoesNotOverwrite == Mode = "add" /\ s.attempts = 1 => s.backed
ExactCachePublication == s.cache = "updated" => s.backed /\ s.marker /\ s.attempts = 1
CacheRequiresDurableMove == s.cache = "updated" => s.synced
UnknownPreservesCache == s.phase = "unknown" => s.cache = "old"
PublicationFinishesRecorded == s.phase = "succeeded" => s.marker /\ s.cache = "updated"
OldAddNeedsOwnCandidate == Mode = "add" /\ s.phase = "not-committed" => s.candidate = "owned" /\ s.attempts = 0
NoWriteFromWrongState == s.attempts = 1 => s.canonicalAtWrite

AbsentSuccess == s.phase # "succeeded"
AbsentBeforeWriteRecovery == s.phase # "not-committed"
AbsentAfterSourceWindow == ~(s.phase = "moved" /\ s.backed /\ s.cache = "old")
AbsentAfterCacheWindow == s.phase # "post"
AbsentDriftAfterPublication == ~(s.phase = "succeeded" /\ ~s.target /\ s.marker)
AbsentTargetDriftHeld == ~(s.phase = "unknown" /\ s.backed /\ ~s.target)
AbsentUnexpectedRetained == ~(s.phase = "unknown" /\ s.holding = "other")
AbsentConsumedAndRemoved == ~(s.phase = "unknown" /\ s.source = "missing" /\ s.candidate = "consumed")
AbsentRestoreWithOtherTreeChange ==
    ~(s.phase = "unknown" /\ s.source = "other" /\ s.holding = "empty" /\ ~s.inputs /\ s.attempts = 1)
AbsentNewFileConflict == ~(s.phase = "unknown" /\ s.attempts = 0 /\ s.source = "other")
===============================================================================
