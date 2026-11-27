----------------------- MODULE SkillSourceSwitch -----------------------
EXTENDS Naturals
VARIABLE state, run
vars == <<state, run>>

Init == /\ state \in {"old", "declared", "bound", "updated"}
        /\ run = [approved |-> FALSE, pending |-> "none", done |-> FALSE,
                 binds |-> 0, upgrades |-> 0]

Confirm == /\ ~run.approved /\ ~run.done
           /\ run' = [run EXCEPT !.approved = TRUE]
           /\ UNCHANGED state
Cancel == /\ ~run.approved /\ ~run.done
          /\ run' = [run EXCEPT !.done = TRUE]
          /\ UNCHANGED state
StartBind == /\ run.approved /\ ~run.done /\ run.pending = "none"
             /\ state \in {"old", "declared"} /\ run.binds < 1
             /\ state' = "declared"
             /\ run' = [run EXCEPT !.pending = "bind", !.binds = @ + 1]
StartUpgrade == /\ run.approved /\ ~run.done /\ run.pending = "none"
                /\ state = "bound" /\ run.upgrades < 1
                /\ run' = [run EXCEPT !.pending = "upgrade", !.upgrades = @ + 1]
                /\ UNCHANGED state
Observe == /\ run.pending # "none"
           /\ \E success \in BOOLEAN:
                state' = IF success THEN
                    IF run.pending = "bind" THEN "bound" ELSE "updated"
                    ELSE state
           /\ run' = [run EXCEPT !.pending = "none"]
Finish == /\ run.approved /\ ~run.done /\ run.pending = "none"
          /\ state = "updated" \/ (state \in {"old", "declared"} /\ run.binds = 1)
                 \/ (state = "bound" /\ run.upgrades = 1)
          /\ run' = [run EXCEPT !.done = TRUE]
          /\ UNCHANGED state
Next == Confirm \/ Cancel \/ StartBind \/ StartUpgrade \/ Observe \/ Finish
        \/ (run.done /\ UNCHANGED vars)
ConfirmedWrites == (run.binds > 0 \/ run.upgrades > 0) => run.approved
NoUpgradeBeforeBinding == run.upgrades > 0 => state \in {"bound", "updated"}
NoPendingOnFinish == run.done => run.pending = "none"
=======================================================================
