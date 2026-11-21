--------------------------- MODULE NpmBinding ---------------------------
EXTENDS Naturals
VARIABLE s
Ends == {"Bound", "Review", "Cancelled", "NotCommitted"}
CanTwist == {"Previewed", "Approved", "Locked"}
Init ==
    \E authority \in BOOLEAN:
        s = [phase |-> "Draft", authority |-> authority, confirmed |-> FALSE,
             oldSame |-> TRUE, scopeSame |-> TRUE, beforeSame |-> TRUE,
             receipt |-> "Observed", publishes |-> 0, publishOk |-> FALSE]
Ready == s.authority /\ s.oldSame /\ s.scopeSame /\ s.beforeSame
Preview ==
    /\ s.phase = "Draft"
    /\ s' = [s EXCEPT !.phase = "Previewed"]
Approve ==
    /\ s.phase = "Previewed" /\ Ready
    /\ s' = [s EXCEPT !.phase = "Approved", !.confirmed = TRUE]
Lock ==
    /\ s.phase = "Approved" /\ Ready
    /\ s' = [s EXCEPT !.phase = "Locked"]
OldChanges ==
    /\ s.phase \in CanTwist /\ s.oldSame
    /\ s' = [s EXCEPT !.oldSame = FALSE]
ScopeChanges ==
    /\ s.phase \in CanTwist /\ s.scopeSame
    /\ s' = [s EXCEPT !.scopeSame = FALSE]
BeforeChanges ==
    /\ s.phase \in CanTwist /\ s.beforeSame
    /\ s' = [s EXCEPT !.beforeSame = FALSE]
AuthorityChanges ==
    /\ s.phase \in CanTwist /\ s.authority
    /\ s' = [s EXCEPT !.authority = FALSE]
Reject ==
    /\ s.phase \in CanTwist /\ ~Ready
    /\ s' = [s EXCEPT !.phase = "Review"]
Cancel ==
    /\ s.phase \in {"Draft"} \cup CanTwist
    /\ s' = [s EXCEPT !.phase = "Cancelled"]
Publish ==
    /\ s.phase = "Locked"
    /\ Ready /\ s.confirmed
    /\ s' = [s EXCEPT !.receipt = "ExactNew", !.publishes = @ + 1,
              !.publishOk = Ready /\ s.confirmed, !.phase = "AwaitTerminal"]
CrashBeforeReceipt ==
    /\ s.phase = "Locked"
    /\ s' = [s EXCEPT !.receipt = IF s.oldSame THEN "ExactOld" ELSE "Other",
              !.phase = "AwaitTerminal"]
UnprovenRecord ==
    /\ s.phase = "AwaitTerminal"
    /\ \E kind \in {"Missing", "Other"}:
         s' = [s EXCEPT !.receipt = kind]
InspectExactNew ==
    /\ s.phase \in {"AwaitTerminal", "Unknown"} /\ s.receipt = "ExactNew"
    /\ s' = [s EXCEPT !.phase = "Bound"]
InspectExactOld ==
    /\ s.phase \in {"AwaitTerminal", "Unknown"} /\ s.receipt = "ExactOld"
    /\ s' = [s EXCEPT !.phase = "NotCommitted"]
InspectUnproven ==
    /\ s.phase = "AwaitTerminal" /\ s.receipt \in {"Missing", "Other"}
    /\ s' = [s EXCEPT !.phase = "Unknown"]
Wait ==
    /\ s.phase \in Ends \cup {"Unknown"}
    /\ UNCHANGED s
Next == Preview \/ Approve \/ Lock \/ OldChanges \/ ScopeChanges \/ BeforeChanges
        \/ AuthorityChanges \/ Reject \/ Cancel \/ Publish \/ CrashBeforeReceipt
        \/ UnprovenRecord \/ InspectExactNew \/ InspectExactOld \/ InspectUnproven \/ Wait
Spec == Init /\ [][Next]_s
SinglePublish == s.publishes <= 1
OnlyAfterConfirmation == s.publishes > 0 => s.publishOk
StillObservedUntilPublication == s.phase \in {"Draft"} \cup CanTwist => s.receipt = "Observed"
RecoveredExactNew == s.phase = "Bound" => s.receipt = "ExactNew" /\ s.publishes = 1
RecoveredExactOld == s.phase = "NotCommitted" => s.receipt = "ExactOld" /\ s.publishes = 0
UnprovenKeepsBlocked == s.receipt \in {"Missing", "Other"} => s.phase \in {"AwaitTerminal", "Unknown"}
ReachBound == s.phase # "Bound"
ReachNotCommitted == s.phase # "NotCommitted"
ReachMissingUnknown == ~(s.phase = "Unknown" /\ s.receipt = "Missing")
ReachOtherUnknown == ~(s.phase = "Unknown" /\ s.receipt = "Other")
ReachOldFence == ~(s.phase = "Review" /\ ~s.oldSame /\ s.confirmed)
ReachScopeFence == ~(s.phase = "Review" /\ ~s.scopeSame /\ s.confirmed)
ReachLiveFence == ~(s.phase = "Review" /\ ~s.beforeSame /\ s.confirmed)
ReachCatalogFence == ~(s.phase = "Review" /\ ~s.authority /\ s.confirmed)
=========================================================================
