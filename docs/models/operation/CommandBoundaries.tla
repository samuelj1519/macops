----------------------------- MODULE CommandBoundaries -----------------------------
EXTENDS Naturals
CONSTANT Legacy
VARIABLE s
Init == \E kind \in {"confirmed", "manual", "diagnose", "batch", "followup"},
          exact \in BOOLEAN, pending \in BOOLEAN:
  s = [kind |-> kind, exact |-> exact, pending |-> pending, phase |-> "start",
       approved |-> FALSE, recovered |-> FALSE, attempted |-> 0,
       stopped |-> FALSE, needNext |-> FALSE, complete |-> FALSE]
Offer == s.phase = "start" /\ s.kind = "confirmed" /\
   s' = [s EXCEPT !.phase = "confirm", !.recovered = Legacy /\ s.pending /\ s.exact]
Approve == s.phase = "confirm" /\
   s' = [s EXCEPT !.phase = "check", !.approved = TRUE]
Cancel == s.phase = "confirm" /\ s' = [s EXCEPT !.phase = "cancelled"]
Manual == s.phase = "start" /\ s.kind = "manual" /\
   s' = [s EXCEPT !.phase = "check", !.approved = TRUE]
Recover == s.phase = "check" /\ s.pending /\ (s.exact \/ (Legacy /\ s.kind = "manual")) /\
   s' = [s EXCEPT !.phase = "done", !.recovered = TRUE]
Blocked == s.phase = "check" /\ s.pending /\ ~s.exact /\
   s' = [s EXCEPT !.phase = "blocked"]
NothingPending == s.phase = "check" /\ ~s.pending /\ s' = [s EXCEPT !.phase = "done"]
Diagnose == s.phase = "start" /\ s.kind = "diagnose" /\
   s' = [s EXCEPT !.phase = IF s.pending /\ Legacy THEN "blocked" ELSE "done"]
FirstMember == s.phase = "start" /\ s.kind = "batch" /\
   s' = [s EXCEPT !.phase = "running", !.attempted = 1]
Stop == s.phase = "running" /\
   s' = [s EXCEPT !.phase = IF Legacy THEN "running" ELSE "done", !.stopped = TRUE]
NextMember == s.phase = "running" /\ s.attempted = 1 /\ (Legacy \/ ~s.stopped) /\
   s' = [s EXCEPT !.phase = "done", !.attempted = 2]
Followup == s.phase = "start" /\ s.kind = "followup" /\
   s' = [s EXCEPT !.phase = "done", !.needNext = TRUE, !.complete = Legacy]
Done == s.phase \in {"done", "blocked", "cancelled"} /\ UNCHANGED s
Next == Offer \/ Approve \/ Cancel \/ Manual \/ Recover \/ Blocked \/ NothingPending \/
   Diagnose \/ FirstMember \/ Stop \/ NextMember \/ Followup \/ Done
ApprovedRecovery == s.recovered => s.approved
SameRecoveryRoots == s.recovered => s.exact
DiagnosticsAvailable == s.kind = "diagnose" => s.phase # "blocked"
StopsBatch == s.stopped => s.attempted = 1
StageIsNotComplete == s.needNext => ~s.complete
=============================================================================
