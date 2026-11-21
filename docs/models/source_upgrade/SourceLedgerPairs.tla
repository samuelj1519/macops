------------------------- MODULE SourceLedgerPairs -------------------------
EXTENDS Naturals, FiniteSets, Sequences, TLC
VARIABLE s
vars == <<s>>
K == 1..5
X == {"target","n","r"}
Slots == K \X X
Nil == "missing"
O(k) == "old-" \o ToString(k)
NC(c,k) == "new-" \o ToString(IF c=5 /\ k=2 THEN 1 ELSE k)
N(k) == NC(s.case,k)
R(k) == "restored-" \o ToString(k)
F(k) == "foreign-same-bytes-mode-" \o ToString(k)
Changed(c) == CASE c=2 -> {2,3,4} [] c=3 -> {1,3,4,5} [] OTHER -> K
Missing(c) == CASE c=2 -> {3} [] c=3 -> {5} [] OTHER -> {}
Old(k) == IF k \in Missing(s.case) THEN Nil ELSE O(k)
At(k,x) == s.files[<<k,x>>]
Objects(f) == {f[p]: p \in Slots} \ {Nil}
Before(k) == IF k \notin Changed(s.case) THEN At(k,"target")=Old(k) /\ At(k,"n")=Nil /\ At(k,"r")=Nil
 ELSE IF k \in Missing(s.case) THEN At(k,"target")=Nil /\ At(k,"n")=N(k) /\ At(k,"r")=Nil
 ELSE (At(k,"target")=O(k) /\ At(k,"n")=N(k) /\ At(k,"r")=Nil /\ k \notin s.savedR)
   \/ (At(k,"target")=R(k) /\ At(k,"n")=O(k) /\ At(k,"r")=N(k) /\ k \in s.savedR)
After(k) == IF k \notin Changed(s.case) THEN Before(k)
 ELSE At(k,"target")=N(k) /\ At(k,"n")=Old(k)
   /\ ((At(k,"r")=Nil /\ k \notin s.savedR) \/ (At(k,"r")=R(k) /\ k \in s.savedR /\ k \notin Missing(s.case)))
Known(k) == Before(k) \/ After(k)
Safe == \A k \in K : Known(k)
AllBefore == (\A k \in K : Before(k)) /\ s.live="O" /\ s.retained="N"
AllAfter == (\A k \in K : After(k)) /\ s.live="N" /\ s.retained="O"
Active == s.phase \in {"running","recover","await-after","await-before"}
InitialFiles(c) == [p \in Slots |-> IF p[2]="target" THEN IF p[1] \in Missing(c) THEN Nil ELSE O(p[1]) ELSE IF p[2]="n" /\ p[1] \in Changed(c) THEN NC(c,p[1]) ELSE Nil]
UniqueRaw == \A p,q \in Slots:p#q /\ s.files[p]#Nil => s.files[p]#s.files[q]
PreflightValid == Cardinality({s.locations[p]:p \in Slots})=15 /\ UniqueRaw
Init == \E c \in 1..5 : s=[case |-> c, files |-> InitialFiles(c), locations |-> [p \in Slots |-> IF c=4 /\ p= <<2,"r">> THEN <<1,"r">> ELSE p],
 live |-> "O",retained |-> "N",phase |-> "plan",pending |-> FALSE,evidenceN |-> FALSE,savedR |-> {},savedMissing |-> {},attemptedR |-> {},volatileAttempt |-> 0,volatileR |-> 0,intent |-> <<"none",0>>,proof |-> "none",crashes |-> 0,external |-> FALSE,lostByEnvironment |-> {},createdR |-> {},initialObjects |-> ({O(k):k \in K \ Missing(c)} \cup {NC(c,k):k \in Changed(c)}),
 forwardF |-> FALSE,backwardF |-> FALSE,sourceF |-> FALSE,orphanCrash |-> FALSE,restoredCrash |-> FALSE,clearAfterCrash |-> FALSE,clearBeforeCrash |-> FALSE,missingForward |-> FALSE,missingRollback |-> FALSE,destinationBlocked |-> FALSE,sourceCalls |-> 0,clearKind |-> "none",published |-> {},rolled |-> {},authorizedR |-> {},orphaned |-> {},rCreates |-> [k \in K |-> 0],exchanges |-> 0,lastCrashExchange |-> 0,postRestoreCrashes |-> 0,clearRCrash |-> FALSE,clearMissingCrash |-> FALSE,rollbackSourceF |-> FALSE,rollbackMissingF |-> FALSE,rollbackBlocked |-> FALSE,blockedLedger |-> 0,blockedPair |-> <<Nil,Nil>>,foreignUsed |-> FALSE,deleted |-> FALSE,deletedAfterClear |-> FALSE]
Prepare == s.phase="plan" /\ PreflightValid /\ Safe /\ s'=[s EXCEPT !.evidenceN=TRUE,!.pending=TRUE,!.phase="running",!.sourceCalls=1]
RejectPlan == s.phase="plan" /\ ~PreflightValid /\ s'=[s EXCEPT !.phase="rejected"]
PublishRoot == s.phase="running" /\ Safe /\ s.live="O" /\ s'=[s EXCEPT !.live="N",!.retained="O"]
CheckForward(k) == s.phase="running" /\ s.pending /\ s.evidenceN /\ Safe /\ s.live="N" /\ s.intent= <<"none",0>> /\ k \in Changed(s.case) /\ Before(k) /\ (\A j \in K:j<k => After(j)) /\ s'=[s EXCEPT !.intent= <<"forward",k>>]
Forward(k) == s.intent= <<"forward",k>> /\ s.phase="running" /\ s.pending /\ s.evidenceN
 /\ s'=[s EXCEPT !.files[<<k,"target">>]=IF k \in Missing(s.case) /\ At(k,"target")#Nil THEN @ ELSE At(k,"n"),
 !.files[<<k,"n">>]=IF k \in Missing(s.case) THEN IF At(k,"target")=Nil THEN Nil ELSE @ ELSE At(k,"target"),!.intent= <<"none",0>>,!.proof="none",
 !.published=IF At(k,"n")=N(k) /\ (k \notin Missing(s.case) \/ At(k,"target")=Nil) THEN @ \cup {k} ELSE @,!.exchanges=@+1,
 !.blockedLedger=IF k \in Missing(s.case) /\ At(k,"target")#Nil THEN k ELSE @,!.blockedPair=IF k \in Missing(s.case) /\ At(k,"target")#Nil THEN <<At(k,"target"),At(k,"n")>> ELSE @,
 !.forwardF=@ \/ (k \notin Missing(s.case) /\ At(k,"target")=F(k)),!.sourceF=@ \/ At(k,"n")=F(k),!.missingForward=@ \/ (k \in Missing(s.case) /\ At(k,"target")=Nil),!.destinationBlocked=@ \/ (k \in Missing(s.case) /\ At(k,"target")#Nil)]
ReserveR(k) == s.phase="recover" /\ Safe /\ ~AllAfter /\ After(k) /\ ~Before(k) /\ k \notin Missing(s.case) /\ k \notin s.attemptedR /\ s.volatileAttempt=0 /\ s.volatileR=0 /\ s.intent= <<"none",0>> /\ (\A j \in K:j>k => Before(j)) /\ s'=[s EXCEPT !.attemptedR=@ \cup {k},!.volatileAttempt=k]
CreateR(k) == s.phase="recover" /\ Safe /\ s.volatileAttempt=k /\ k \in s.attemptedR /\ k \notin s.savedR /\ s.rCreates[k]=0 /\ At(k,"r")=Nil /\ s'=[s EXCEPT !.files[<<k,"r">>]=R(k),!.volatileR=k,!.volatileAttempt=0,!.createdR=@ \cup {R(k)},!.rCreates[k]=@+1]
SaveR(k) == s.phase="recover" /\ s.volatileR=k /\ At(k,"r")=R(k) /\ At(k,"target")=N(k) /\ At(k,"n")=O(k) /\ s'=[s EXCEPT !.savedR=@ \cup {k},!.authorizedR=@ \cup {k},!.volatileR=0]
SaveMissing(k) == s.phase="recover" /\ Safe /\ ~AllAfter /\ After(k) /\ ~Before(k) /\ k \in Missing(s.case) /\ k \notin s.savedMissing /\ (\A j \in K:j>k => Before(j)) /\ s'=[s EXCEPT !.savedMissing=@ \cup {k}]
CheckRollback(k) == s.phase="recover" /\ Safe /\ ~AllAfter /\ ~Before(k) /\ After(k) /\ s.intent= <<"none",0>> /\ (IF k \in Missing(s.case) THEN k \in s.savedMissing ELSE k \in s.savedR) /\ (\A j \in K:j>k => Before(j)) /\ s'=[s EXCEPT !.intent= <<"rollback",k>>]
Rollback(k) == s.phase="recover" /\ s.intent= <<"rollback",k>> /\ (IF k \in Missing(s.case) THEN k \in s.savedMissing ELSE k \in s.savedR)
 /\ s'=[s EXCEPT !.files[<<k,"target">>]=IF k \in Missing(s.case) THEN IF At(k,"n")=Nil THEN Nil ELSE @ ELSE At(k,"r"),!.files[<<k,"r">>]=IF k \in Missing(s.case) THEN @ ELSE At(k,"target"),!.files[<<k,"n">>]=IF k \in Missing(s.case) /\ At(k,"n")=Nil THEN At(k,"target") ELSE @,!.intent= <<"none",0>>,!.proof="none",
 !.rolled=IF At(k,"target")=N(k) /\ (IF k \in Missing(s.case) THEN At(k,"n")=Nil ELSE At(k,"r")=R(k)) THEN @ \cup {k} ELSE @,!.exchanges=@+1,
 !.rollbackSourceF=@ \/ (k \notin Missing(s.case) /\ At(k,"r")=F(k)),!.rollbackMissingF=@ \/ (k \in Missing(s.case) /\ At(k,"target")=F(k) /\ At(k,"n")=Nil),
 !.rollbackBlocked=@ \/ (k \in Missing(s.case) /\ At(k,"n")#Nil),!.blockedLedger=IF k \in Missing(s.case) /\ At(k,"n")#Nil THEN k ELSE @,!.blockedPair=IF k \in Missing(s.case) /\ At(k,"n")#Nil THEN <<At(k,"target"),At(k,"n")>> ELSE @,
 !.backwardF=@ \/ At(k,"target")=F(k),!.sourceF=@ \/ At(k,"r")=F(k),!.missingRollback=@ \/ (k \in Missing(s.case) /\ At(k,"n")=Nil),!.destinationBlocked=@ \/ (k \in Missing(s.case) /\ At(k,"n")#Nil)]
RestoreRoot == s.phase="recover" /\ Safe /\ ~AllAfter /\ (\A k \in K:Before(k)) /\ s.live="N" /\ s'=[s EXCEPT !.live="O",!.retained="N"]
VerifyAfter == s.phase \in {"running","recover"} /\ Safe /\ AllAfter /\ s'=[s EXCEPT !.proof="after"]
ClearAfter == s.phase \in {"running","recover"} /\ Safe /\ AllAfter /\ s.proof="after" /\ s'=[s EXCEPT !.pending=FALSE,!.phase="await-after",!.clearKind="after"]
Done == s.phase="await-after" /\ Safe /\ AllAfter /\ s.proof="after" /\ s'=[s EXCEPT !.phase="done"]
VerifyBefore == s.phase="recover" /\ Safe /\ AllBefore /\ s'=[s EXCEPT !.proof="before"]
ClearBefore == s.phase="recover" /\ Safe /\ AllBefore /\ s.proof="before" /\ s'=[s EXCEPT !.pending=FALSE,!.phase="await-before",!.clearKind="before"]
Failed == s.phase="await-before" /\ Safe /\ AllBefore /\ s.proof="before" /\ s'=[s EXCEPT !.phase="rolled-back"]
Crash == Active /\ s.crashes<3 /\ s'=[s EXCEPT !.phase="recover",!.crashes=@+1,!.intent= <<"none",0>>,!.proof="none",!.orphanCrash=@ \/ (s.attemptedR \ s.savedR#{}),!.orphaned=@ \cup (s.attemptedR \ s.savedR),!.volatileR=0,!.volatileAttempt=0,
 !.postRestoreCrashes=IF (\E k \in s.rolled:At(k,"target")=R(k)) THEN IF s.lastCrashExchange=s.exchanges THEN @+1 ELSE 1 ELSE @,!.lastCrashExchange=s.exchanges,
 !.clearRCrash=@ \/ (s.phase="await-before" /\ s.rolled \ Missing(s.case)#{}),!.clearMissingCrash=@ \/ (s.phase="await-before" /\ s.rolled \cap Missing(s.case)#{}),!.restoredCrash=@ \/ (\E k \in K:At(k,"target")=R(k)),!.clearAfterCrash=@ \/ s.phase="await-after",!.clearBeforeCrash=@ \/ s.phase="await-before"]
External(k,x) == Active /\ ~s.external /\ s'=[s EXCEPT !.files[<<k,x>>]=F(k),!.external=TRUE,!.foreignUsed=TRUE,!.proof="none",!.lostByEnvironment=IF At(k,x)=Nil THEN @ ELSE @ \cup {At(k,x)}]
DeleteRetained(k,x) == Active /\ ~s.external /\ x \in {"n","r"} /\ At(k,x)#Nil /\ s'=[s EXCEPT !.files[<<k,x>>]=Nil,!.external=TRUE,!.deleted=TRUE,!.deletedAfterClear=(s.clearKind#"none"),!.proof="none",!.lostByEnvironment=@ \cup {At(k,x)}]
Block == Active /\ ((~Safe /\ (s.volatileR=0 \/ s.external)) \/ (s.attemptedR \ s.savedR#{} /\ s.volatileR=0 /\ s.volatileAttempt=0)) /\ s.intent= <<"none",0>> /\ s'=[s EXCEPT !.phase="unknown",!.proof="none"]
Terminal == s.phase \in {"done","rolled-back","unknown","rejected"} /\ UNCHANGED vars
Progress == RejectPlan \/ Prepare \/ PublishRoot \/ (\E k \in K:CheckForward(k) \/ Forward(k) \/ ReserveR(k) \/ CreateR(k) \/ SaveR(k) \/ SaveMissing(k) \/ CheckRollback(k) \/ Rollback(k)) \/ RestoreRoot \/ VerifyAfter \/ ClearAfter \/ Done \/ VerifyBefore \/ ClearBefore \/ Failed \/ Block
Next == Progress \/ Crash \/ (\E k \in K,x \in X:External(k,x) \/ DeleteRetained(k,x)) \/ Terminal
Spec == Init /\ [][Next]_vars /\ WF_vars(Progress)
RawAfter == \A k \in K: IF k \in Changed(s.case) THEN At(k,"target")=N(k) /\ At(k,"n")=Old(k) /\ ((At(k,"r")=Nil /\ k \notin s.savedR) \/ (At(k,"r")=R(k) /\ k \in s.savedR)) ELSE At(k,"target")=Old(k) /\ At(k,"n")=Nil /\ At(k,"r")=Nil
RawBefore == \A k \in K: IF k \in Changed(s.case) THEN IF k \in Missing(s.case) THEN At(k,"target")=Nil /\ At(k,"n")=N(k) /\ At(k,"r")=Nil ELSE (At(k,"target")=O(k) /\ At(k,"n")=N(k) /\ At(k,"r")=Nil /\ k \notin s.savedR) \/ (At(k,"target")=R(k) /\ At(k,"n")=O(k) /\ At(k,"r")=N(k) /\ k \in s.savedR) ELSE At(k,"target")=Old(k) /\ At(k,"n")=Nil /\ At(k,"r")=Nil
RawTerminalAfter == s.phase="done" => ~s.pending /\ RawAfter /\ s.live="N" /\ s.retained="O"
RawTerminalBefore == s.phase="rolled-back" => ~s.pending /\ RawBefore /\ s.live="O" /\ s.retained="N"
ProductPreservesObjects == (s.initialObjects \cup s.createdR) \ s.lostByEnvironment \subseteq Objects(s.files)
ForeignPreserved == s.foreignUsed => \E k \in K:F(k) \in Objects(s.files)
UniqueObjects == s.evidenceN => UniqueRaw
RecordedBeforePublish == (\E k \in K:At(k,"target")=N(k)) => s.evidenceN
RestoredRecorded == \A k \in K:At(k,"target")=R(k) => k \in s.savedR
DurableEvidenceAfterClear == s.clearKind="before" => \A k \in K:At(k,"target")=R(k) => k \in s.savedR
UnchangedUntouched == ~s.external => \A k \in K \ Changed(s.case):At(k,"target")=Old(k) /\ At(k,"n")=Nil /\ At(k,"r")=Nil
CommitPrefix == ~s.external => \A k \in Changed(s.case):At(k,"target")=N(k) => \A j \in Changed(s.case):j<k => At(j,"target")=N(j)
RootAfterLedgerRollback == ~s.external /\ s.live="O" => \A k \in K:At(k,"target")#N(k)
NoSourceInRecovery == s.evidenceN => s.sourceCalls=1
EventuallyTerminal == <>(s.phase \in {"done","rolled-back","unknown","rejected"})
NoForwardForeign == ~(s.phase="unknown" /\ s.forwardF)
NoRollbackForeign == ~(s.phase="unknown" /\ s.backwardF)
NoSourceForeign == ~(s.phase="unknown" /\ s.sourceF)
NoOrphanCrash == ~(s.phase="unknown" /\ s.orphanCrash)
NoRestoredCrash == ~(s.phase="rolled-back" /\ s.postRestoreCrashes>=2 /\ s.rolled \ Missing(s.case)#{})
NoClearAfterCrash == ~(s.phase="done" /\ s.clearAfterCrash)
NoClearBeforeCrash == ~(s.phase="rolled-back" /\ s.clearRCrash)
NoMissingRoundtrip == ~(s.phase="rolled-back" /\ s.missingForward /\ s.missingRollback)
NoDestinationBlocked == ~(s.phase="unknown" /\ s.destinationBlocked)

ClearKeepsN == s.clearKind#"none" => s.evidenceN
RollbackEvidenceDurable == s.rolled \ Missing(s.case) \subseteq s.savedR /\ s.rolled \cap Missing(s.case) \subseteq s.savedMissing
RProvenance == s.savedR \subseteq s.authorizedR /\ s.orphaned \cap s.savedR={}
OrphanPreserved == \A k \in s.orphaned: s.rCreates[k]=0 \/ At(k,"r")=R(k) \/ R(k) \in s.lostByEnvironment
OrphanNeverPublished == \A k \in s.orphaned:At(k,"target")#R(k)
OrphanNeverSettles == s.orphaned#{} => s.phase \notin {"done","rolled-back"}
OneRCreation == \A k \in K:s.rCreates[k]<=1
NoReplaceCollisionPreservesPair == s.blockedLedger#0 => <<At(s.blockedLedger,"target"),At(s.blockedLedger,"n")>>=s.blockedPair
NamespacesSeparated == s.evidenceN => Cardinality({s.locations[p]:p \in Slots})=15
RejectedBeforeEffects == s.phase="rejected" => s.files=InitialFiles(s.case) /\ s.live="O" /\ s.retained="N" /\ ~s.pending /\ ~s.evidenceN /\ s.sourceCalls=0 /\ s.published={} /\ s.rolled={} /\ s.savedR={} /\ s.savedMissing={}
NoClearMissingCrash == ~(s.phase="rolled-back" /\ s.clearMissingCrash)
NoRollbackSourceForeign == ~(s.phase="unknown" /\ s.rollbackSourceF)
NoMissingRollbackForeign == ~(s.phase="unknown" /\ s.rollbackMissingF)
NoRollbackDestinationBlocked == ~(s.phase="unknown" /\ s.rollbackBlocked /\ s.blockedLedger#0 /\ At(s.blockedLedger,"target")=N(s.blockedLedger) /\ At(s.blockedLedger,"n")=F(s.blockedLedger))
NoPathCollisionRejected == ~(s.phase="rejected" /\ s.case=4)
NoIdentityCollisionRejected == ~(s.phase="rejected" /\ s.case=5)
NoRetainedDeletedBeforeClear == ~(s.phase="unknown" /\ s.deleted /\ s.pending /\ s.published#{})
NoRetainedDeletedAfterClear == ~(s.phase="unknown" /\ s.deletedAfterClear /\ ~s.pending /\ s.published#{})
=============================================================================
