------------------------------ MODULE Replay -------------------------------
EXTENDS Naturals
VARIABLE s
vars == <<s>>
Bits == {TRUE,FALSE}
Authority == s.formal /\ s.scopePresent /\ s.scope /\ s.authority /\ s.exactVersion
Ready == Authority /\ s.observed /\ ~s.pending
Init ==
    \E formal, scopePresent, scope, authority, exactVersion, observed, dry, pending, exactReceipt, equalVersion \in Bits:
      s = [formal |-> formal, scopePresent |-> scopePresent, scope |-> scope, authority |-> authority,
           exactVersion |-> exactVersion, observed |-> observed, dry |-> dry, pending |-> pending,
           exactReceipt |-> exactReceipt, equalVersion |-> equalVersion, phase |-> "preview",
           gate |-> FALSE, sends |-> 0, nativeEvidence |-> FALSE]
NoChange ==
    /\ s.phase = "preview" /\ Ready /\ s.exactReceipt /\ s.equalVersion
    /\ s' = [s EXCEPT !.phase = "unchanged"]
Confirm ==
    /\ s.phase = "preview" /\ ~s.dry /\ Ready /\ ~s.equalVersion
    /\ s' = [s EXCEPT !.phase = "confirmed"]
Drift ==
    /\ s.phase = "confirmed"
    /\ \/ s' = [s EXCEPT !.observed = FALSE]
       \/ s' = [s EXCEPT !.formal = FALSE]
       \/ s' = [s EXCEPT !.scope = FALSE]
       \/ s' = [s EXCEPT !.pending = TRUE]
Gate ==
    /\ s.phase = "confirmed" /\ Ready
    /\ s' = [s EXCEPT !.phase = "running", !.gate = Ready, !.sends = @ + 1]
Stop ==
    /\ s.phase = "running"
    /\ \E checked \in Bits:
         s' = [s EXCEPT !.phase = IF checked THEN "done" ELSE "unknown",
                         !.nativeEvidence = checked, !.pending = ~checked]
Next == NoChange \/ Confirm \/ Drift \/ Gate \/ Stop
OnlyAdmittedLaunch == s.sends > 0 => s.gate /\ ~s.dry
OnlyOneLaunch == s.sends \in 0..1
FinalRequiresNativeProof == s.phase = "done" => s.nativeEvidence /\ s.sends = 1
UnchangedNeedsProof == s.phase = "unchanged" => Authority /\ s.observed /\ ~s.pending /\ s.exactReceipt /\ s.equalVersion /\ s.sends = 0
AbsentDone == s.phase # "done"
AbsentUnknown == s.phase # "unknown"
AbsentDrift == ~(s.phase = "confirmed" /\ ~s.formal)
AbsentNoChange == s.phase # "unchanged"
=============================================================================
