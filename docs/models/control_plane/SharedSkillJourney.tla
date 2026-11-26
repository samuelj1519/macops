-------------------------- MODULE SharedSkillJourney --------------------------
VARIABLE s

Init ==
    \E owned \in BOOLEAN, preflight \in BOOLEAN, current \in BOOLEAN:
        s = [phase |-> "menu", owned |-> owned, preflight |-> preflight, current |-> current,
             adoptApproved |-> FALSE, upgradeApproved |-> FALSE,
             adopted |-> FALSE, upgradeStarted |-> FALSE]

Review ==
    /\ s.phase = "menu"
    /\ s' = [s EXCEPT !.phase = IF s.owned THEN "check-version"
                                 ELSE IF s.preflight THEN "confirm-adopt" ELSE "blocked"]

Cancel ==
    /\ s.phase \in {"menu", "confirm-adopt", "confirm-upgrade"}
    /\ s' = [s EXCEPT !.phase = "cancelled"]

Adopt ==
    /\ s.phase = "confirm-adopt"
    /\ s' = [s EXCEPT !.phase = "adopting", !.adoptApproved = TRUE]

FinishAdopt ==
    /\ s.phase = "adopting"
    /\ s' = [s EXCEPT !.phase = "check-version", !.owned = TRUE, !.adopted = TRUE]

CheckVersion ==
    /\ s.phase = "check-version"
    /\ s' = [s EXCEPT !.phase = IF s.current THEN "current"
                                 ELSE IF s.preflight THEN "confirm-upgrade" ELSE "blocked"]

Upgrade ==
    /\ s.phase = "confirm-upgrade"
    /\ s' = [s EXCEPT !.phase = "upgrading", !.upgradeApproved = TRUE, !.upgradeStarted = TRUE]

FinishUpgrade == s.phase = "upgrading" /\ s' = [s EXCEPT !.phase = "updated"]
Stop == s.phase \in {"adopting", "upgrading"} /\ s' = [s EXCEPT !.phase = "stopped"]
Done == s.phase \in {"blocked", "updated", "cancelled", "current", "stopped"} /\ UNCHANGED s

Next == Review \/ Cancel \/ Adopt \/ FinishAdopt \/ CheckVersion \/ Upgrade \/ FinishUpgrade \/ Stop \/ Done

AdoptionRequiresApproval == s.adopted => s.adoptApproved
UpgradeRequiresOwnerAndApproval == s.upgradeStarted => s.owned /\ s.upgradeApproved
PreflightBeforeConfirmation == s.phase \in {"confirm-adopt", "confirm-upgrade"} => s.preflight
CancelKeepsPayload == s.phase = "cancelled" => ~s.upgradeStarted
CurrentKeepsPayload == s.phase = "current" => s.current /\ ~s.upgradeStarted
==============================================================================
