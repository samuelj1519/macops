-------------------------- MODULE InstallAdmission --------------------------
EXTENDS Naturals
VARIABLE s

Eligible == {"Brew", "Npm"}
Candidates == Eligible \cup {"Unproven", "Conflict", "Unsupported"}
Ends == {"Done", "Failed", "Cancelled", "Stale", "Review"}
Settled == Ends \cup {"Unknown"}
PreLaunch == {"Proposed", "Approved", "Prepared"}
Phases == Settled \cup {"Draft", "Proposed", "Approved", "Prepared", "Running", "Checked"}

Init ==
    \E scope \in Eligible:
        s = [phase |-> "Draft", candidate |-> "None", scope |-> scope,
             epoch |-> 0, approved |-> "None", approvalEpoch |-> 0,
             before |-> "Match", impact |-> "Allowed", runs |-> 0,
             launchOk |-> FALSE, exitProof |-> "None", post |-> "None",
             postNow |-> "None", receipt |-> FALSE]

ScopeMatches == s.scope = s.approved /\ s.epoch = s.approvalEpoch
BeforeMatches == s.before = "Match"
ImpactAllowed == s.impact = "Allowed"
CanDispatch == ScopeMatches /\ BeforeMatches /\ ImpactAllowed

Propose ==
    /\ s.phase = "Draft"
    /\ \E c \in Candidates:
         s' = [s EXCEPT !.phase = "Proposed", !.candidate = c]

ReviewOnly ==
    /\ s.phase = "Proposed"
    /\ s.candidate \notin Eligible
    /\ s' = [s EXCEPT !.phase = "Review"]

Approve ==
    /\ s.phase = "Proposed"
    /\ s.candidate \in Eligible
    /\ s.candidate = s.scope
    /\ BeforeMatches /\ ImpactAllowed
    /\ s' = [s EXCEPT !.phase = "Approved", !.approved = s.candidate,
              !.approvalEpoch = s.epoch]

Prepare ==
    /\ s.phase = "Approved"
    /\ CanDispatch
    /\ s' = [s EXCEPT !.phase = "Prepared"]

ScopeChange ==
    /\ s.phase \notin Ends
    /\ s.epoch < 2
    /\ \E v \in Eligible \ {s.scope}:
         s' = [s EXCEPT !.scope = v, !.epoch = @ + 1]

BeforeChange ==
    /\ s.phase \in PreLaunch
    /\ BeforeMatches
    /\ s' = [s EXCEPT !.before = "Drift"]

ImpactChange ==
    /\ s.phase \in PreLaunch
    /\ ImpactAllowed
    /\ \E impact \in {"Outside", "Unknown"}:
         s' = [s EXCEPT !.impact = impact]

MarkStale ==
    /\ s.phase \in PreLaunch
    /\ s.candidate \in Eligible
    /\ (s.scope # s.candidate
        \/ (s.phase # "Proposed" /\ s.epoch # s.approvalEpoch)
        \/ ~BeforeMatches \/ ~ImpactAllowed)
    /\ s' = [s EXCEPT !.phase = "Stale"]

Cancel ==
    /\ s.phase \in {"Draft"} \cup PreLaunch
    /\ s' = [s EXCEPT !.phase = "Cancelled"]

Dispatch ==
    /\ s.phase = "Prepared"
    /\ s.approved \in Eligible
    /\ s.approved = s.candidate
    /\ ScopeMatches
    /\ BeforeMatches
    /\ ImpactAllowed
    /\ s' = [s EXCEPT !.phase = "Running", !.runs = @ + 1, !.before = "Consumed",
              !.launchOk = s.approved \in Eligible /\ s.approved = s.candidate /\ CanDispatch]

ReadOriginalOutcome ==
    /\ s.phase \in {"Running", "Unknown"}
    /\ \E proof \in {"Original", "Other", "None"}, post \in {"Match", "Rejected", "None"}:
         LET next == IF proof = "Original" /\ ScopeMatches
                        THEN CASE post = "Match" -> "Checked"
                               [] post = "Rejected" -> "Failed"
                               [] OTHER -> "Unknown"
                        ELSE "Unknown"
         IN s' = [s EXCEPT !.phase = next, !.exitProof = proof, !.post = post, !.postNow = post]

AfterChange ==
    /\ s.phase = "Checked"
    /\ s.postNow = "Match"
    /\ s' = [s EXCEPT !.postNow = "Drift"]

Publish ==
    /\ s.phase = "Checked"
    /\ ScopeMatches
    /\ s.postNow = "Match"
    /\ s' = [s EXCEPT !.phase = "Done", !.receipt = TRUE]

PublicationUncertain ==
    /\ s.phase = "Checked"
    /\ (~ScopeMatches \/ s.postNow # "Match")
    /\ s' = [s EXCEPT !.phase = "Unknown"]

WaitForEvidence ==
    /\ s.phase \in Settled
    /\ UNCHANGED s

Next == Propose \/ ReviewOnly \/ Approve \/ Prepare \/ ScopeChange \/ BeforeChange
        \/ ImpactChange \/ MarkStale \/ Cancel \/ Dispatch \/ ReadOriginalOutcome
        \/ AfterChange \/ Publish \/ PublicationUncertain \/ WaitForEvidence

Spec == Init /\ [][Next]_s

TypeOk ==
    /\ s.phase \in Phases
    /\ s.candidate \in Candidates \cup {"None"}
    /\ s.scope \in Eligible
    /\ s.approved \in Eligible \cup {"None"}
    /\ s.epoch \in 0..2
    /\ s.approvalEpoch \in 0..2
    /\ s.before \in {"Match", "Drift", "Consumed"}
    /\ s.impact \in {"Allowed", "Outside", "Unknown"}
    /\ s.runs \in Nat
    /\ s.launchOk \in BOOLEAN
    /\ s.exitProof \in {"Original", "Other", "None"}
    /\ s.post \in {"Match", "Rejected", "None"}
    /\ s.postNow \in {"Match", "Rejected", "None", "Drift"}
    /\ s.receipt \in BOOLEAN
AdmittedDispatch == s.runs > 0 => s.launchOk
AtMostOneDispatch == s.runs <= 1
ReceiptHasProof == s.receipt =>
    s.phase = "Done" /\ s.runs = 1 /\ s.launchOk /\ ScopeMatches
    /\ s.exitProof = "Original" /\ s.post = "Match" /\ s.postNow = "Match"
CanReachSuccess == ~(s.phase = "Done" /\ s.before = "Consumed")
CanReachUnknown == s.phase # "Unknown"
CanReachStale == s.phase # "Stale"
CanReachReview == s.phase # "Review"
CanReachCancelled == s.phase # "Cancelled"
CanReachFailed == s.phase # "Failed"
CanReachBeforeDrift == ~(s.phase = "Stale" /\ s.before = "Drift")
CanReachImpactOutside == ~(s.phase = "Stale" /\ s.impact = "Outside")
CanReachImpactUnknown == ~(s.phase = "Stale" /\ s.impact = "Unknown")
CanReachLateAfterDrift == ~(s.phase = "Unknown" /\ s.postNow = "Drift")
CanReachScopeABA == ~(s.phase = "Stale" /\ s.approved \in Eligible
                      /\ s.scope = s.approved /\ s.epoch # s.approvalEpoch)

Progress ==
    Propose \/ ReviewOnly \/ Approve \/ Prepare \/ MarkStale \/ Cancel \/ Dispatch
    \/ (ReadOriginalOutcome /\ s.phase = "Running")
    \/ Publish \/ PublicationUncertain
FairSpec == Spec /\ WF_s(Progress)
Settles == <>(s.phase \in Settled)
=============================================================================
