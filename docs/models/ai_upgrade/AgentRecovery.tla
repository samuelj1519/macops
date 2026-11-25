-------------------------- MODULE AgentRecovery --------------------------
CONSTANT Legacy
VARIABLE s
Ends == {"pending", "drift", "plan", "running", "profileChanged", "evidenceMissing", "verified"}
Init ==
    \E action \in {"upgrade", "recover"}, tool \in {"codex", "claude"},
       pending \in BOOLEAN, drift \in BOOLEAN, forceLive \in BOOLEAN,
       processAlive \in BOOLEAN, profileChanged \in BOOLEAN, proven \in BOOLEAN:
      /\ (action = "upgrade" => ~processAlive /\ ~profileChanged /\ ~proven)
      /\ (action = "recover" => pending /\ ~drift /\ ~forceLive)
      /\ s = [phase |-> "start", action |-> action, tool |-> tool, pending |-> pending,
               drift |-> drift, forceLive |-> forceLive, processAlive |-> processAlive,
               profileChanged |-> profileChanged, proven |-> proven, sourceWrite |-> FALSE]
BlockedByDrift == s.drift /\ ~s.forceLive
Upgrade ==
    /\ s.phase = "start" /\ s.action = "upgrade"
    /\ s' = [s EXCEPT !.phase = IF Legacy /\ BlockedByDrift THEN "drift"
                                 ELSE IF s.pending THEN "pending"
                                 ELSE IF BlockedByDrift THEN "drift" ELSE "plan"]
CheckRecovery ==
    /\ s.phase = "start" /\ s.action = "recover"
    /\ s' = [s EXCEPT !.phase = IF s.processAlive THEN "running"
                ELSE IF s.profileChanged /\ (Legacy \/ s.tool = "claude") THEN "profileChanged"
                ELSE IF s.proven THEN "verified" ELSE "evidenceMissing",
           !.sourceWrite = s.proven /\ ~s.processAlive /\ ~(s.profileChanged /\ (Legacy \/ s.tool = "claude"))]
Done == s.phase \in Ends /\ UNCHANGED s
Next == Upgrade \/ CheckRecovery \/ Done

PendingIsFirst == s.action = "upgrade" /\ s.pending => s.phase \in {"start", "pending"}
OtherToolIgnoresClaude == s.tool # "claude" => s.phase # "profileChanged"
ChangedClaudeNeverPublishes == s.tool = "claude" /\ s.profileChanged => ~s.sourceWrite
NeedsOriginalEvidence == s.sourceWrite => s.action = "recover" /\ ~s.processAlive /\ s.proven
PendingNeverPlans == s.phase = "plan" => ~s.pending
==========================================================================
