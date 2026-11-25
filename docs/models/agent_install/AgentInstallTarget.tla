------------------------- MODULE AgentInstallTarget -------------------------
EXTENDS Naturals
VARIABLE s
Targets == {"one", "two"}
EndStates == {"cancelled", "refused", "complete"}
Init == \E enrolled \in {{}, {"one"}}, pending \in Targets \cup {"none"}:
  s = [phase |-> "list", selected |-> "none", initial |-> enrolled,
       enrolled |-> enrolled, agreed |-> FALSE, valid |-> TRUE, completed |-> 0,
       writes |-> {}, running |-> FALSE, frozen |-> FALSE, published |-> 0,
       original |-> pending, pending |-> pending]
Choose == s.phase = "list" /\ \E target \in Targets:
  s' = [s EXCEPT !.selected = target,
    !.phase = "confirm"]
Register == s.phase = "register" /\ s.valid /\ s.agreed /\
  s' = [s EXCEPT !.enrolled = @ \cup {s.selected}, !.phase = "ready"]
Cancel == s.phase \in {"list", "confirm"} /\
  s' = [s EXCEPT !.phase = "cancelled"]
Confirm == s.phase = "confirm" /\ s.valid /\
  s' = [s EXCEPT !.agreed = TRUE, !.phase = "reconcile"]
Reconcile == s.phase = "reconcile" /\ s.valid /\ s.pending \in {"none", s.selected} /\
  s' = [s EXCEPT !.pending = "none", !.phase = IF s.selected \in s.enrolled THEN "ready" ELSE "register"]
Blocked == s.phase = "reconcile" /\ s.pending # "none" /\
  s' = [s EXCEPT !.phase = "refused"]
Drift == s.phase \in {"confirm", "reconcile", "register", "ready", "checking", "publishReady", "publishing", "publishPaused"} /\ s.valid /\
  s' = [s EXCEPT !.valid = FALSE]
Refuse == s.phase \in {"confirm", "reconcile", "register", "ready", "checking", "publishReady", "publishing", "publishPaused"} /\ ~s.valid /\
  s' = [s EXCEPT !.phase = "refused"]
Start == s.phase = "ready" /\ s.valid /\ s.completed < 3 /\
  s' = [s EXCEPT !.phase = "native", !.running = TRUE]
Exit == s.phase = "native" /\
  s' = [s EXCEPT !.phase = "checking", !.running = FALSE]
LoseAnswer == s.phase = "native" /\
  s' = [s EXCEPT !.phase = "waiting"]
OriginalExit == s.phase = "waiting" /\
  s' = [s EXCEPT !.phase = "checking", !.running = FALSE]
Verify == s.phase = "checking" /\ s.valid /\ ~s.running /\
  s' = [s EXCEPT !.completed = @ + 1, !.phase = IF s.completed = 2 THEN "publishReady" ELSE "ready"]
Indeterminate == s.phase = "checking" /\ ~s.running /\
  s' = [s EXCEPT !.phase = "refused"]
Freeze == s.phase = "publishReady" /\ s.valid
  /\ s' = [s EXCEPT !.frozen = TRUE, !.phase = "publishing"]
Apply == s.phase = "publishing" /\ s.valid /\ s.published < 2
  /\ s' = [s EXCEPT !.published = @ + 1, !.writes = @ \cup {s.selected},
    !.phase = IF s.published = 1 THEN "complete" ELSE "publishing"]
Interrupt == s.phase = "publishing" /\ s.valid
  /\ s' = [s EXCEPT !.phase = "publishPaused"]
Resume == s.phase = "publishPaused" /\ s.valid
  /\ s' = [s EXCEPT !.phase = "publishing"]
Done == s.phase \in EndStates /\ UNCHANGED s
Next == Choose \/ Register \/ Cancel \/ Confirm \/ Reconcile \/ Blocked \/ Drift \/ Refuse \/ Start \/ Exit \/
  LoseAnswer \/ OriginalExit \/ Verify \/ Indeterminate \/ Freeze \/ Apply \/ Interrupt \/ Resume \/ Done
OnlySelected == s.writes \subseteq {s.selected}
ConfirmedWrites == s.writes # {} => s.agreed /\ s.selected \in s.enrolled
ListAndCancelAreReadOnly == s.phase \in {"list", "cancelled"} => s.writes = {} /\ s.enrolled = s.initial
RegistrationIsExplicit == s.enrolled # s.initial => s.agreed /\ (s.enrolled \ s.initial) \subseteq {s.selected}
RecoveryMatchesSelection == s.pending # s.original => s.agreed /\ s.original = s.selected
EnrolledAfterRecovery == s.enrolled # s.initial => s.pending = "none"
NoOverlap == s.running => s.phase \in {"native", "waiting"}
ValidFinish == s.phase = "complete" => s.completed = 3 /\ s.published = 2 /\ s.valid /\ ~s.running
PublishedOnlyAfterFreeze == s.published > 0 => s.frozen /\ s.completed = 3 /\ ~s.running
NoNativeProfileWrites == s.phase \in {"native","waiting"} => s.writes = {}
=============================================================================
