-------------------------- MODULE SourceUpgrade --------------------------
EXTENDS Naturals, FiniteSets, Sequences, TLC
VARIABLE s
vars == <<s>>
Roots == {"alpha", "beta"}
Ledgers == 1..5
None == "none"
Old(r) == "old-" \o r
New(r) == "new-" \o r
Foreign(r) == "foreign-" \o r
Changed(c) == CASE c = 1 -> Ledgers [] c = 2 -> {1,3,4,5} [] c = 3 -> {2,3,4,5} [] c = 4 -> {3,5} [] c = 6 -> {2,3,4} [] OTHER -> {}
Missing(c) == CASE c \in {3,6} -> {1,5} [] c = 4 -> {3,4,5} [] OTHER -> {}
Mode(c) == IF c \in {4,5} THEN "adopt" ELSE "upgrade"
BeforeValue(k) == IF k \in Missing(s.scenario) THEN "missing" ELSE "before"
BeforeId(k) == IF k \in Missing(s.scenario) THEN "missing" ELSE "before-id"
BeforeLedger(k) == s.ledgers[k].value = BeforeValue(k)
    /\ (s.ledgers[k].id = BeforeId(k) \/ (k \notin Missing(s.scenario) /\ k \in s.savedRollback /\ s.ledgers[k].id = "restored-id"))
AfterLedger(k) == IF k \in Changed(s.scenario) THEN s.ledgers[k] = [value |-> "after", id |-> "after-id"] ELSE BeforeLedger(k)
KnownLedger(k) == BeforeLedger(k) \/ AfterLedger(k)
StageBefore(r) == IF r \in s.selected /\ Mode(s.scenario) = "upgrade" THEN New(r) ELSE None
BeforeRoot(r) == s.live[r] = Old(r) /\ s.retained[r] = StageBefore(r)
AfterRoot(r) == IF r \in s.selected /\ Mode(s.scenario) = "upgrade" THEN s.live[r] = New(r) /\ s.retained[r] = Old(r) ELSE BeforeRoot(r)
KnownRoot(r) == BeforeRoot(r) \/ AfterRoot(r)
AllBefore == (\A r \in Roots : BeforeRoot(r)) /\ (\A k \in Ledgers : BeforeLedger(k))
AllAfter == (\A r \in Roots : AfterRoot(r)) /\ (\A k \in Ledgers : AfterLedger(k))
Safe == s.parentsValid /\ (\A r \in Roots : KnownRoot(r)) /\ (\A k \in Ledgers : KnownLedger(k))
Active == s.phase \in {"plan","running","recover","await-success","await-before"}
Init == \E c \in 1..6, chosen \in {{"alpha"},Roots} :
    s = [scenario |-> c, selected |-> chosen,
        live |-> [r \in Roots |-> Old(r)],
        retained |-> [r \in Roots |-> IF r \in chosen /\ Mode(c) = "upgrade" THEN New(r) ELSE None],
        ledgers |-> [k \in Ledgers |-> [value |-> IF k \in Missing(c) THEN "missing" ELSE "before", id |-> IF k \in Missing(c) THEN "missing" ELSE "before-id"]],
        evidence |-> FALSE, pending |-> FALSE, phase |-> "plan", intent |-> None,
        savedRollback |-> {}, proof |-> None, crashes |-> 0, externalUsed |-> FALSE,
        foreignRoot |-> None, oldLostByEnvironment |-> {}, afterClearCrash |-> FALSE, parentsValid |-> TRUE, readSourceCalls |-> 0, callsAtRecovery |-> 0, nativePartialCrash |-> FALSE, adoptPartialCrash |-> FALSE,
        lateCASRoot |-> None, lateCASSwapped |-> FALSE, restoredCrash |-> FALSE, upgradeClearCrash |-> FALSE]
Prepare == s.phase = "plan" /\ Safe /\ s' = [s EXCEPT !.evidence = TRUE, !.pending = TRUE, !.phase = "running", !.readSourceCalls = @ + 1]
CheckRoot(r) == s.phase = "running" /\ Safe /\ s.intent = None /\ Mode(s.scenario) = "upgrade" /\ r \in s.selected /\ BeforeRoot(r)
    /\ s' = [s EXCEPT !.intent = r]
SwapRoot(r) == s.phase = "running" /\ s.pending /\ s.evidence /\ s.intent = r
    /\ s' = [s EXCEPT !.live[r] = s.retained[r], !.retained[r] = s.live[r], !.intent = None, !.proof = None, !.lateCASSwapped = @ \/ s.lateCASRoot = r]
PublishLedger(k) == s.phase = "running" /\ s.pending /\ Safe /\ (\A r \in Roots : AfterRoot(r))
    /\ k \in Changed(s.scenario) /\ BeforeLedger(k) /\ (\A j \in Ledgers : j < k => AfterLedger(j))
    /\ s' = [s EXCEPT !.ledgers[k] = [value |-> "after", id |-> "after-id"], !.proof = None]
VerifyAfter == s.phase \in {"running","recover"} /\ Safe /\ AllAfter /\ s' = [s EXCEPT !.proof = "after"]
ClearAfter == s.phase \in {"running","recover"} /\ Safe /\ AllAfter /\ s.proof = "after"
    /\ s' = [s EXCEPT !.pending = FALSE, !.phase = "await-success"]
SaveSuccess == s.phase = "await-success" /\ Safe /\ AllAfter /\ s.proof = "after" /\ s' = [s EXCEPT !.phase = "done"]
Crash == s.phase \in {"running","recover","await-success","await-before"} /\ s.crashes < 2
    /\ s' = [s EXCEPT !.phase = "recover", !.intent = None, !.crashes = @ + 1, !.proof = None, !.callsAtRecovery = s.readSourceCalls, !.afterClearCrash = @ \/ ~s.pending,
        !.nativePartialCrash = @ \/ (s.scenario = 1 /\ s.ledgers[1].id = "after-id" /\ s.ledgers[2].id = "before-id"),
        !.adoptPartialCrash = @ \/ (s.scenario = 4 /\ s.ledgers[3].id = "after-id" /\ s.ledgers[5].id = "missing"),
        !.restoredCrash = @ \/ (\E k \in Ledgers : s.ledgers[k].id = "restored-id"),
        !.upgradeClearCrash = @ \/ (s.scenario = 1 /\ ~s.pending /\ s.phase = "await-success")]
SaveRollback(k) == s.phase = "recover" /\ Safe /\ ~AllAfter /\ ~BeforeLedger(k)
    /\ k \notin Missing(s.scenario) /\ k \notin s.savedRollback /\ (\A j \in Ledgers : j > k => BeforeLedger(j))
    /\ s' = [s EXCEPT !.savedRollback = @ \cup {k}]
RestoreLedger(k) == s.phase = "recover" /\ Safe /\ ~AllAfter /\ ~BeforeLedger(k)
    /\ (k \in Missing(s.scenario) \/ k \in s.savedRollback) /\ (\A j \in Ledgers : j > k => BeforeLedger(j))
    /\ s' = [s EXCEPT !.ledgers[k] = [value |-> BeforeValue(k), id |-> IF k \in Missing(s.scenario) THEN "missing" ELSE "restored-id"], !.proof = None]
RestoreRoot(r) == s.phase = "recover" /\ Safe /\ ~AllAfter /\ (\A k \in Ledgers : BeforeLedger(k)) /\ ~BeforeRoot(r) /\ AfterRoot(r)
    /\ s' = [s EXCEPT !.live[r] = s.retained[r], !.retained[r] = s.live[r], !.proof = None]
VerifyBefore == s.phase = "recover" /\ Safe /\ AllBefore /\ ~AllAfter /\ s' = [s EXCEPT !.proof = "before"]
ClearBefore == s.phase = "recover" /\ Safe /\ AllBefore /\ ~AllAfter /\ s.proof = "before"
    /\ s' = [s EXCEPT !.pending = FALSE, !.phase = "await-before"]
SaveBefore == s.phase = "await-before" /\ Safe /\ AllBefore /\ s.proof = "before" /\ s' = [s EXCEPT !.phase = "rolled-back"]
Block == Active /\ ~Safe /\ s' = [s EXCEPT !.phase = "unknown", !.intent = None, !.proof = None]
ExternalRoot(r,location) == Active /\ ~s.externalUsed /\ r \in s.selected
    /\ s' = [s EXCEPT !.live[r] = IF location = "live" THEN Foreign(r) ELSE @,
                    !.retained[r] = IF location = "retained" THEN Foreign(r) ELSE @,
                    !.externalUsed = TRUE, !.foreignRoot = r, !.proof = None,
                    !.lateCASRoot = IF location = "live" /\ s.intent = r THEN r ELSE @,
                    !.oldLostByEnvironment = IF (location = "live" /\ s.live[r] = Old(r)) \/ (location = "retained" /\ s.retained[r] = Old(r)) THEN @ \cup {r} ELSE @]
ExternalLedger(k) == Active /\ ~s.externalUsed
    /\ s' = [s EXCEPT !.ledgers[k] = [value |-> BeforeValue(k), id |-> "external-id"], !.externalUsed = TRUE, !.proof = None]
ExternalParent == Active /\ ~s.externalUsed /\ s' = [s EXCEPT !.parentsValid = FALSE, !.externalUsed = TRUE, !.proof = None]
Progress == Prepare \/ (\E r \in Roots : CheckRoot(r) \/ SwapRoot(r) \/ RestoreRoot(r))
    \/ (\E k \in Ledgers : PublishLedger(k) \/ SaveRollback(k) \/ RestoreLedger(k))
    \/ VerifyAfter \/ ClearAfter \/ SaveSuccess \/ VerifyBefore \/ ClearBefore \/ SaveBefore \/ Block
Terminal == s.phase \in {"done","rolled-back","unknown"} /\ UNCHANGED vars
Next == Terminal \/ Progress \/ Crash \/ (\E r \in Roots, location \in {"live","retained"} : ExternalRoot(r,location)) \/ (\E k \in Ledgers : ExternalLedger(k)) \/ ExternalParent
Spec == Init /\ [][Next]_vars /\ WF_vars(Progress)
SuccessIsComplete == s.phase = "done" => ~s.pending /\ Safe /\ AllAfter /\ s.proof = "after"
RollbackIsComplete == s.phase = "rolled-back" => ~s.pending /\ Safe /\ AllBefore /\ s.proof = "before"
UnselectedRootsUntouched == \A r \in Roots \ s.selected : BeforeRoot(r)
CandidateRecordedBeforePublication == (\E r \in Roots : s.live[r] = New(r)) => s.evidence
RestoredInodeWasRecorded == \A k \in Ledgers : s.ledgers[k].id = "restored-id" => k \in s.savedRollback /\ s.ledgers[k].value = BeforeValue(k)
ForeignDirectoryRetained == s.foreignRoot # None => Foreign(s.foreignRoot) \in {s.live[s.foreignRoot],s.retained[s.foreignRoot]}
RecoveryNeverRunsSource == s.crashes > 0 => s.readSourceCalls = s.callsAtRecovery
PartialNeverHealthy == s.phase = "done" => AllAfter
AdoptionNeverSwaps == Mode(s.scenario) = "adopt" => \A r \in Roots : s.live[r] # New(r)
NoExternalSuccess == s.phase = "done" => ~s.externalUsed
NativeReceiptMatches == s.phase = "done" /\ 1 \in Changed(s.scenario) => s.ledgers[1] = [value |-> "after", id |-> "after-id"]
ReceiptCommitComplete == s.phase = "done" /\ 5 \in Changed(s.scenario) => s.ledgers[5] = [value |-> "after", id |-> "after-id"]
OriginalRootRetained == \A r \in Roots : r \notin s.oldLostByEnvironment => Old(r) \in {s.live[r],s.retained[r]}
EventuallyTerminal == <>(s.phase \in {"done","rolled-back","unknown"})
NoUpgradeSuccess == ~(s.phase = "done" /\ Mode(s.scenario) = "upgrade")
NoAdoptionSuccess == ~(s.phase = "done" /\ Mode(s.scenario) = "adopt" /\ s.scenario = 4)
NoNativePartialRecovery == ~(s.phase = "rolled-back" /\ s.nativePartialCrash)
NoOwnRestoredBefore == ~(s.phase = "rolled-back" /\ s.savedRollback # {})
NoCrashAfterClearSuccess == ~(s.phase = "done" /\ s.afterClearCrash)
NoForeignRootUnknown == ~(s.phase = "unknown" /\ s.foreignRoot # None)
NoUnknownInodeBlocked == ~(s.phase = "unknown" /\ \E k \in Ledgers : s.ledgers[k].id = "external-id")
NoUnchangedLedgerSuccess == ~(s.phase = "done" /\ s.scenario = 5)

NoLateCASForeignRetained == ~(s.phase = "unknown" /\ s.lateCASSwapped /\ \E r \in Roots : s.live[r] = New(r) /\ s.retained[r] = Foreign(r))
NoAdoptPartialRollback == ~(s.phase = "rolled-back" /\ s.adoptPartialCrash)
NoRepeatedOwnRestoreRecovery == ~(s.phase = "rolled-back" /\ s.restoredCrash)

LiteralLedgerCompletion == s.phase = "done" => \A k \in Ledgers :
    IF k \in Changed(s.scenario) THEN s.ledgers[k] = [value |-> "after", id |-> "after-id"]
    ELSE s.ledgers[k].value = (IF k \in Missing(s.scenario) THEN "missing" ELSE "before")
         /\ (s.ledgers[k].id = (IF k \in Missing(s.scenario) THEN "missing" ELSE "before-id")
             \/ (k \in s.savedRollback /\ s.ledgers[k].id = "restored-id"))
LiteralCommitPrefix == ~s.externalUsed => \A k \in Changed(s.scenario) : s.ledgers[k].id = "after-id" =>
    \A j \in Changed(s.scenario) : j < k => s.ledgers[j].id = "after-id"
LiteralRootCompletion == s.phase = "done" => \A r \in Roots :
    IF r \in s.selected /\ Mode(s.scenario) = "upgrade"
    THEN s.live[r] = New(r) /\ s.retained[r] = Old(r)
    ELSE s.live[r] = Old(r) /\ s.retained[r] = None
NoUpgradeAfterClearRecovery == ~(s.phase = "done" /\ s.upgradeClearCrash)
NoDeclaredWithoutReceiptSuccess == ~(s.phase = "done" /\ s.scenario = 6 /\ s.ledgers[5].id = "missing")

LiteralRollbackRootCompletion == s.phase = "rolled-back" => \A r \in Roots :
    s.live[r] = Old(r) /\ s.retained[r] = (IF r \in s.selected /\ Mode(s.scenario) = "upgrade" THEN New(r) ELSE None)
LiteralRollbackLedgerCompletion == s.phase = "rolled-back" => \A k \in Ledgers :
    s.ledgers[k].value = (IF k \in Missing(s.scenario) THEN "missing" ELSE "before")
    /\ (s.ledgers[k].id = (IF k \in Missing(s.scenario) THEN "missing" ELSE "before-id")
         \/ (k \notin Missing(s.scenario) /\ k \in s.savedRollback /\ s.ledgers[k].id = "restored-id"))
=============================================================================
