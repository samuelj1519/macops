------------------------- MODULE RecommendJourney -------------------------
EXTENDS Naturals
CONSTANT Legacy
VARIABLE s
Ends == {"advice", "cancelled", "blocked", "complete", "failed", "resume-visible", "resume-hidden", "setup-done"}
Init ==
    \E mode \in {"recommend", "setup"}, count \in {0, 1, 2}, pending \in BOOLEAN:
        s = [mode |-> mode, phase |-> "start", count |-> count, pending |-> pending,
             shown |-> FALSE, selected |-> FALSE, permitted |-> FALSE,
             dispatched |-> FALSE, invalid |-> FALSE, recoverable |-> FALSE]
Setup == s.phase = "start" /\ s.mode = "setup" /\ s' = [s EXCEPT !.phase = "setup-done"]
AdviceOnly == s.phase = "start" /\ s.mode = "recommend" /\ s.count = 0
              /\ s' = [s EXCEPT !.phase = "advice"]
ShowOptions ==
    /\ s.phase = "start" /\ s.mode = "recommend" /\ s.count > 0
    /\ s' = [s EXCEPT !.shown = ~(Legacy /\ s.pending),
          !.phase = IF s.pending THEN "blocked"
                    ELSE IF Legacy /\ s.count = 1 THEN "confirm" ELSE "picker"]
InvalidNumber == s.phase = "picker" /\ ~s.invalid
                 /\ s' = [s EXCEPT !.invalid = TRUE, !.phase = IF Legacy THEN "cancelled" ELSE "picker"]
Cancel == s.phase \in {"picker", "confirm"} /\ s' = [s EXCEPT !.phase = "cancelled", !.invalid = FALSE]
Select == s.phase = "picker" /\ ~s.pending
          /\ s' = [s EXCEPT !.phase = "confirm", !.selected = TRUE, !.invalid = FALSE]
PendingAppears == s.phase \in {"picker", "confirm"} /\ ~s.pending /\ s' = [s EXCEPT !.pending = TRUE]
Reject == s.phase \in {"picker", "confirm"} /\ s.pending
          /\ s' = [s EXCEPT !.phase = "blocked", !.invalid = FALSE]
Confirm == s.phase = "confirm" /\ ~s.pending
           /\ s' = [s EXCEPT !.phase = "running", !.permitted = TRUE, !.dispatched = TRUE]
Finish == s.phase = "running"
          /\ \E result \in {"complete", "failed", "unknown"}:
              s' = [s EXCEPT !.phase = result, !.recoverable = result = "unknown"]
Recover ==
    /\ s.phase = "unknown"
    /\ \E result \in {"complete", "unknown", "resume"}:
        s' = [s EXCEPT !.phase = IF result # "resume" THEN result
                               ELSE IF Legacy THEN "resume-hidden" ELSE "resume-visible"]
Done == s.phase \in Ends /\ UNCHANGED s
Next == Setup \/ AdviceOnly \/ ShowOptions \/ InvalidNumber \/ Cancel \/ Select \/ PendingAppears
        \/ Reject \/ Confirm \/ Finish \/ Recover \/ Done

BlockedStillShowsOptions == s.phase = "blocked" => s.shown
InvalidStaysInPicker == s.invalid => s.phase = "picker"
SelectedBeforeConfirm == s.phase = "confirm" => s.selected
ConfirmedDispatch == s.dispatched => s.permitted /\ ~s.pending
RecoveryNeverHidesContinuation == s.phase # "resume-hidden"
SetupIndependent == s.mode = "setup" => ~s.dispatched /\ s.phase \in {"start", "setup-done"}
===========================================================================
