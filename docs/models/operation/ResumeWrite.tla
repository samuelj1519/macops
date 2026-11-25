---------------------------- MODULE ResumeWrite ----------------------------
VARIABLE s
Ends == {"read", "wait", "blocked", "oldDone", "newDone"}
Init ==
    \E request \in {"read", "same", "different"}, pending \in BOOLEAN,
       matchesScope \in BOOLEAN, alive \in BOOLEAN, proved \in BOOLEAN,
       json \in BOOLEAN:
         s = [phase |-> "start", request |-> request, pending |-> pending,
              matchesScope |-> matchesScope, alive |-> alive, proved |-> proved,
              json |-> json, oldWrites |-> FALSE, newRuns |-> FALSE,
              printed |-> FALSE]
Read == s.phase = "start" /\ s.request = "read" /\ s' = [s EXCEPT !.phase = "read"]
Ready == s.phase = "start" /\ s.request # "read" /\ ~s.pending
         /\ s' = [s EXCEPT !.phase = "newReady"]
Review == s.phase = "start" /\ s.request # "read" /\ s.pending
          /\ s' = [s EXCEPT !.phase = "checking"]
Wait == s.phase = "checking" /\ s.alive /\ s' = [s EXCEPT !.phase = "wait"]
Refuse == s.phase = "checking" /\ ~s.alive /\ (~s.matchesScope \/ ~s.proved)
          /\ s' = [s EXCEPT !.phase = "blocked"]
SettleOld == s.phase = "checking" /\ ~s.alive /\ s.matchesScope /\ s.proved
             /\ s' = [s EXCEPT !.phase = IF s.request = "same" THEN "oldDone" ELSE "newReady",
                           !.pending = FALSE, !.oldWrites = TRUE]
StartNew == s.phase = "newReady" /\ ~s.pending
            /\ s' = [s EXCEPT !.phase = "newRunning", !.newRuns = TRUE]
Progress == s.phase = "newRunning" /\ ~s.json /\ s' = [s EXCEPT !.printed = TRUE]
Finish == s.phase = "newRunning" /\ s' = [s EXCEPT !.phase = "newDone"]
Done == s.phase \in Ends /\ UNCHANGED s
Next == Read \/ Ready \/ Review \/ Wait \/ Refuse \/ SettleOld \/ StartNew \/ Progress \/ Finish \/ Done
ReadsStayReadOnly == s.request = "read" => ~s.oldWrites /\ ~s.newRuns
AuthorizedOldWrites == s.oldWrites => s.matchesScope /\ ~s.alive /\ s.proved
NoDuplicate == s.request = "same" /\ s.oldWrites => ~s.newRuns
NoNewWhilePending == s.newRuns => ~s.pending
MachineOutputClean == s.json => ~s.printed
============================================================================
