--------------------------- MODULE QueryLifetime ---------------------------
CONSTANT Legacy
VARIABLE s
Init == \E peer \in BOOLEAN:
    s = [phase |-> "running", peer |-> peer, owned |-> TRUE,
         expired |-> FALSE, signaled |-> FALSE, accepted |-> FALSE]
ParentExit == s.phase = "running" /\
    s' = [s EXCEPT !.phase = "inspect", !.owned = ~Legacy]
PeerExit == s.phase = "inspect" /\ s.peer /\ ~s.expired /\
    s' = [s EXCEPT !.peer = FALSE]
Finish == s.phase = "inspect" /\ ~s.peer /\ ~s.expired /\
    s' = [s EXCEPT !.phase = "done", !.owned = FALSE, !.accepted = TRUE]
Expire == s.phase \in {"running", "inspect"} /\ ~s.expired /\
    s' = [s EXCEPT !.phase = IF Legacy /\ s.phase = "inspect" THEN "waiting" ELSE "cancel",
                   !.expired = TRUE]
Terminate == s.phase = "cancel" /\ s.owned /\
    s' = [s EXCEPT !.phase = "done", !.owned = FALSE,
                   !.signaled = s.owned, !.peer = FALSE]
AbortObservation == s.phase = "inspect" /\ ~s.expired /\
    s' = [s EXCEPT !.phase = IF s.owned THEN "cancel" ELSE "done"]
LatePeer == s.phase = "waiting" /\
    s' = [s EXCEPT !.phase = "done", !.peer = FALSE]
Done == s.phase = "done" /\ UNCHANGED s
Next == ParentExit \/ PeerExit \/ Finish \/ Expire \/ Terminate \/ AbortObservation \/ LatePeer \/ Done
NoExpiredWaiting == s.expired => s.phase \in {"cancel", "done"}
KeepOwnerUntilDecision == s.phase \in {"running", "inspect", "cancel"} => s.owned
SuccessfulExitIsQuiescent == s.accepted => ~s.peer /\ ~s.expired
=============================================================================
