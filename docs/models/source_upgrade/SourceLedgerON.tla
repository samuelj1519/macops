--------------------------- MODULE SourceLedgerON ---------------------------
EXTENDS Naturals, FiniteSets, Sequences, TLC
VARIABLE s
vars == <<s>>
K == 1..5
Sides == {"target","staged"}
Slots == K \X Sides
Physical == 1..14
Nil == "missing"
Changed(c) == IF c=2 THEN {2,3,4} ELSE K
Missing(c) == IF c \in {2,3} THEN {3} ELSE {}
Selected(c) == IF c=2 THEN {"alpha"} ELSE {"alpha","beta"}
OC(c,k) == "old-" \o ToString(IF c=5 /\ k=2 THEN 1 ELSE k)
O(k) == IF k \in Missing(s.case) THEN Nil ELSE OC(s.case,k)
N(k) == "new-" \o ToString(k)
F(p) == "foreign-same-content-mode-" \o ToString(p)
Base(k,x) == IF x="target" THEN 2*k-1 ELSE 2*k
RootLive(r) == IF r="alpha" THEN 11 ELSE 13
RootStage(r) == RootLive(r)+1
Roots == {"alpha","beta"}
RootOld(r) == "old-root-" \o r
RootNew(r) == "new-root-" \o r
Location(c,k,x) == IF c=4 /\ k=1 /\ x="staged" THEN 4 ELSE IF c=9 /\ k=1 /\ x="staged" THEN 11 ELSE Base(k,x)
At(k,x) == s.objects[s.locations[<<k,x>>]]
Objects(o) == {o[p]:p \in Physical} \ {Nil}
Initial(c) == [p \in Physical |-> IF p<=10 THEN IF p%2=0 THEN Nil ELSE LET k==(p+1)\div 2 IN IF k \in Missing(c) THEN Nil ELSE OC(c,k)
    ELSE IF p=11 THEN RootOld("alpha") ELSE IF p=13 THEN RootOld("beta") ELSE IF p=12 THEN RootNew("alpha") ELSE IF "beta" \in Selected(c) THEN RootNew("beta") ELSE Nil]
UniqueRaw == \A p,q \in Physical:p#q /\ s.objects[p]#Nil => s.objects[p]#s.objects[q]
Preflight == Cardinality({s.locations[x]:x \in Slots})=10 /\ {s.locations[x]:x \in Slots} \cap {11,12,13,14}={} /\ UniqueRaw /\ s.sameVolume /\ s.parentsValid /\ s.budgetValid
RootBefore(r) == s.objects[RootLive(r)]=RootOld(r) /\ s.objects[RootStage(r)]=(IF r \in Selected(s.case) THEN RootNew(r) ELSE Nil)
RootAfter(r) == IF r \in Selected(s.case) THEN s.objects[RootLive(r)]=RootNew(r) /\ s.objects[RootStage(r)]=RootOld(r) ELSE RootBefore(r)
Before(k) == At(k,"target")=O(k) /\ At(k,"staged")=(IF k \in Changed(s.case) THEN N(k) ELSE Nil)
After(k) == IF k \in Changed(s.case) THEN At(k,"target")=N(k) /\ At(k,"staged")=O(k) ELSE Before(k)
Known(k) == Before(k) \/ After(k)
CandidateDurable == \A k \in Changed(s.case):s.records[k]=N(k)
Safe == CandidateDurable /\ s.parentsValid /\ (\A k \in K:Known(k)) /\ (\A r \in Roots:RootBefore(r) \/ RootAfter(r))
AllBefore == (\A k \in K:Before(k)) /\ (\A r \in Roots:RootBefore(r))
AllAfter == (\A k \in K:After(k)) /\ (\A r \in Roots:RootAfter(r))
Active == s.phase \in {"running","recover","await-after","await-before"}
Init == \E c \in 1..9:s=[case |-> c,objects |-> Initial(c),locations |-> [x \in Slots |-> Location(c,x[1],x[2])],
    sameVolume |-> c#6,parentsValid |-> c#8,budgetValid |-> c#7,phase |-> "plan",records |-> [k \in K |-> Nil],pending |-> FALSE,started |-> FALSE,
    createCalls |-> 0,freezeCalls |-> 0,createsAfterStart |-> 0,registersAfterStart |-> 0,intent |-> <<"none",0>>,proof |-> "none",crashes |-> 0,
    foreignUsed |-> FALSE,environmentUsed |-> FALSE,lostByEnvironment |-> {},affected |-> {},lostProof |-> {},published |-> {},reversed |-> {},reverseCount |-> 0,lastCrashReverse |-> 0,postReverseCrashes |-> 0,
    completeRecovery |-> FALSE,reverseAtComplete |-> 0,clearKind |-> "none",clearAfterCrash |-> FALSE,clearBeforeCrash |-> FALSE,
    window |-> "none",windowLedger |-> 0,windowExecuted |-> FALSE,blockedPair |-> <<Nil,Nil>>,blockedLedger |-> 0,deleteBeforeClear |-> FALSE,deleteAfterClear |-> FALSE,evidenceDeleted |-> FALSE]
Reject == s.phase="plan" /\ ~Preflight /\ s'=[s EXCEPT !.phase="rejected"]
BuildCandidates == s.phase="plan" /\ Preflight /\ s'=[s EXCEPT !.objects=[p \in Physical |-> IF \E k \in Changed(s.case):p=s.locations[<<k,"staged">>] THEN "new-" \o ToString((p+1)\div 2) ELSE s.objects[p]],!.createCalls=Cardinality(Changed(s.case)),!.phase="built"]
FreezeCandidates == s.phase="built" /\ Preflight /\ (\A k \in K:Before(k)) /\ s'=[s EXCEPT !.records=[k \in K |-> IF k \in Changed(s.case) THEN N(k) ELSE Nil],!.freezeCalls=1,!.phase="ready"]
JournalStart == s.phase="ready" /\ CandidateDurable /\ (\A k \in K:Before(k)) /\ (\A r \in Roots:RootBefore(r)) /\ s'=[s EXCEPT !.started=TRUE,!.pending=TRUE,!.phase="running"]
PublishRoot(r) == s.phase="running" /\ Safe /\ r \in Selected(s.case) /\ RootBefore(r) /\ (r="alpha" \/ RootAfter("alpha"))
    /\ s'=[s EXCEPT !.objects[RootLive(r)]=s.objects[RootStage(r)],!.objects[RootStage(r)]=s.objects[RootLive(r)]]
CheckForward(k) == s.phase="running" /\ Safe /\ s.intent= <<"none",0>> /\ k \in Changed(s.case) /\ Before(k) /\ (\A r \in Roots:RootAfter(r)) /\ (\A j \in K:j<k => After(j)) /\ s'=[s EXCEPT !.intent= <<"forward",k>>]
Forward(k) == s.phase="running" /\ s.intent= <<"forward",k>> /\ s.started /\ s.pending
    /\ s'=[s EXCEPT !.objects[s.locations[<<k,"target">>]]=IF O(k)=Nil /\ At(k,"target")#Nil THEN @ ELSE At(k,"staged"),
        !.objects[s.locations[<<k,"staged">>]]=IF O(k)=Nil THEN IF At(k,"target")=Nil THEN Nil ELSE @ ELSE At(k,"target"),
        !.windowExecuted=@ \/ (s.windowLedger=k /\ s.window#"none"),!.published=IF At(k,"staged")=N(k) /\ At(k,"target")=O(k) THEN @ \cup {k} ELSE @,!.intent= <<"none",0>>,!.proof="none",
        !.blockedLedger=IF O(k)=Nil /\ At(k,"target")#Nil THEN k ELSE @,!.blockedPair=IF O(k)=Nil /\ At(k,"target")#Nil THEN <<At(k,"target"),At(k,"staged")>> ELSE @]
CheckReverse(k) == s.phase="recover" /\ Safe /\ ~AllAfter /\ s.intent= <<"none",0>> /\ k \in Changed(s.case) /\ ~Before(k) /\ After(k) /\ (\A j \in K:j>k => Before(j)) /\ s'=[s EXCEPT !.intent= <<"reverse",k>>]
Reverse(k) == s.phase="recover" /\ s.intent= <<"reverse",k>> /\ s.started
    /\ s'=[s EXCEPT !.objects[s.locations[<<k,"target">>]]=IF O(k)=Nil THEN IF At(k,"staged")=Nil THEN Nil ELSE @ ELSE At(k,"staged"),
        !.objects[s.locations[<<k,"staged">>]]=IF O(k)=Nil /\ At(k,"staged")#Nil THEN @ ELSE At(k,"target"),
        !.windowExecuted=@ \/ (s.windowLedger=k /\ s.window#"none"),!.reversed=IF At(k,"target")=N(k) /\ At(k,"staged")=O(k) THEN @ \cup {k} ELSE @,!.reverseCount=@+1,!.intent= <<"none",0>>,!.proof="none",
        !.blockedLedger=IF O(k)=Nil /\ At(k,"staged")#Nil THEN k ELSE @,!.blockedPair=IF O(k)=Nil /\ At(k,"staged")#Nil THEN <<At(k,"target"),At(k,"staged")>> ELSE @]
RestoreRoot(r) == s.phase="recover" /\ Safe /\ ~AllAfter /\ (\A k \in K:Before(k)) /\ ~RootBefore(r) /\ RootAfter(r) /\ (r="beta" \/ RootBefore("beta"))
    /\ s'=[s EXCEPT !.objects[RootLive(r)]=s.objects[RootStage(r)],!.objects[RootStage(r)]=s.objects[RootLive(r)]]
VerifyAfter == s.phase \in {"running","recover"} /\ Safe /\ AllAfter /\ s'=[s EXCEPT !.proof="after"]
ClearAfter == s.phase \in {"running","recover"} /\ Safe /\ AllAfter /\ s.proof="after" /\ s'=[s EXCEPT !.pending=FALSE,!.phase="await-after",!.clearKind="after"]
SaveAfter == s.phase="await-after" /\ Safe /\ AllAfter /\ s.proof="after" /\ s'=[s EXCEPT !.phase="done"]
VerifyBefore == s.phase="recover" /\ Safe /\ AllBefore /\ s'=[s EXCEPT !.proof="before"]
ClearBefore == s.phase="recover" /\ Safe /\ AllBefore /\ s.proof="before" /\ s'=[s EXCEPT !.pending=FALSE,!.phase="await-before",!.clearKind="before"]
SaveBefore == s.phase="await-before" /\ Safe /\ AllBefore /\ s.proof="before" /\ s'=[s EXCEPT !.phase="rolled-back"]
Crash == Active /\ s.crashes<4 /\ s'=[s EXCEPT !.phase="recover",!.crashes=@+1,!.intent= <<"none",0>>,!.proof="none",
    !.postReverseCrashes=IF s.reversed#{} THEN IF s.lastCrashReverse=s.reverseCount THEN @+1 ELSE 1 ELSE @,!.lastCrashReverse=s.reverseCount,
    !.completeRecovery=@ \/ AllAfter,!.reverseAtComplete=IF AllAfter /\ ~s.completeRecovery THEN s.reverseCount ELSE @,
    !.clearAfterCrash=@ \/ s.phase="await-after",!.clearBeforeCrash=@ \/ (s.phase="await-before" /\ Cardinality(s.published)>=3 /\ s.reversed=s.published /\ s.reversed \cap Missing(s.case)#{})]
External(k,x) == Active /\ ~s.environmentUsed /\ s'=[s EXCEPT !.objects[s.locations[<<k,x>>]]=F(s.locations[<<k,x>>]),!.environmentUsed=TRUE,!.foreignUsed=TRUE,!.affected=@ \cup {k},!.proof="none",
    !.lostByEnvironment=IF At(k,x)=Nil THEN @ ELSE @ \cup {At(k,x)},!.window=IF s.intent[2]=k THEN s.intent[1] \o "-" \o (IF O(k)=Nil THEN "missing-" ELSE "existing-") \o x ELSE "none",!.windowLedger=k]
DeleteRetained(k) == Active /\ ~s.environmentUsed /\ At(k,"staged")#Nil /\ s'=[s EXCEPT !.objects[s.locations[<<k,"staged">>]]=Nil,!.environmentUsed=TRUE,!.affected=@ \cup {k},!.lostByEnvironment=@ \cup {At(k,"staged")},!.proof="none",!.deleteBeforeClear=s.pending /\ s.published#{},!.deleteAfterClear=~s.pending /\ s.published#{}]
LoseEvidence(k) == Active /\ ~s.environmentUsed /\ k \in Changed(s.case) /\ s'=[s EXCEPT !.records[k]=Nil,!.lostProof=@ \cup {k},!.environmentUsed=TRUE,!.proof="none",!.evidenceDeleted=TRUE]
Block == Active /\ ~Safe /\ s.intent= <<"none",0>> /\ s'=[s EXCEPT !.phase="unknown",!.proof="none"]
Terminal == s.phase \in {"done","rolled-back","unknown","rejected"} /\ UNCHANGED vars
Progress == Reject \/ BuildCandidates \/ FreezeCandidates \/ JournalStart \/ (\E r \in Roots:PublishRoot(r) \/ RestoreRoot(r)) \/ (\E k \in K:CheckForward(k) \/ Forward(k) \/ CheckReverse(k) \/ Reverse(k)) \/ VerifyAfter \/ ClearAfter \/ SaveAfter \/ VerifyBefore \/ ClearBefore \/ SaveBefore \/ Block
Next == Progress \/ Crash \/ (\E k \in K,x \in Sides:External(k,x)) \/ (\E k \in K:DeleteRetained(k) \/ LoseEvidence(k)) \/ Terminal
Spec == Init /\ [][Next]_vars /\ WF_vars(Progress)
RawCandidates == \A k \in Changed(s.case):s.records[k]="new-" \o ToString(k)
RawAfter == (\A k \in K:IF k \in Changed(s.case) THEN At(k,"target")="new-" \o ToString(k) /\ At(k,"staged")=(IF k \in Missing(s.case) THEN Nil ELSE OC(s.case,k)) ELSE At(k,"target")=O(k) /\ At(k,"staged")=Nil)
    /\ (\A r \in Roots:IF r \in Selected(s.case) THEN s.objects[RootLive(r)]=RootNew(r) /\ s.objects[RootStage(r)]=RootOld(r) ELSE s.objects[RootLive(r)]=RootOld(r) /\ s.objects[RootStage(r)]=Nil)
RawBefore == (\A k \in K:At(k,"target")=(IF k \in Missing(s.case) THEN Nil ELSE OC(s.case,k)) /\ At(k,"staged")=(IF k \in Changed(s.case) THEN "new-" \o ToString(k) ELSE Nil))
    /\ (\A r \in Roots:s.objects[RootLive(r)]=RootOld(r) /\ s.objects[RootStage(r)]=(IF r \in Selected(s.case) THEN RootNew(r) ELSE Nil))
SuccessComplete == s.phase="done" => ~s.pending /\ RawAfter /\ RawCandidates
RollbackComplete == s.phase="rolled-back" => ~s.pending /\ RawBefore /\ RawCandidates
EvidenceStable == s.started => \A k \in Changed(s.case) \ s.lostProof:s.records[k]="new-" \o ToString(k)
NeverRebindCandidate == \A k \in K:s.records[k] \in {Nil,"new-" \o ToString(k)}
NeverRelearnLostProof == \A k \in s.lostProof:s.records[k]=Nil
PrepareBeforeJournal == s.started => s.createCalls=Cardinality(Changed(s.case)) /\ s.freezeCalls=1
RecoveryNeverBuilds == s.createsAfterStart=0 /\ s.registersAfterStart=0
ProductPreservesObjects == (Objects(Initial(s.case)) \cup (IF s.createCalls>0 THEN {"new-" \o ToString(k):k \in Changed(s.case)} ELSE {})) \ s.lostByEnvironment \subseteq Objects(s.objects)
ForeignPreserved == s.foreignUsed => \E p \in Physical:F(p) \in Objects(s.objects)
UniqueAuthorized == s.started => UniqueRaw
CommitPrefix == ~s.environmentUsed /\ s.started => \A k \in Changed(s.case):At(k,"target")="new-" \o ToString(k) => \A j \in Changed(s.case):j<k => At(j,"target")="new-" \o ToString(j)
ReverseStaysBefore == \A k \in s.reversed \ s.affected:At(k,"target")=(IF k \in Missing(s.case) THEN Nil ELSE OC(s.case,k)) /\ At(k,"staged")="new-" \o ToString(k)
RootsAfterLedgers == s.started /\ ~s.environmentUsed => ((\E r \in Selected(s.case):s.objects[RootLive(r)]=RootOld(r)) => (s.published={} \/ s.reversed=s.published))
CompleteRecoveryNeverReverses == s.completeRecovery /\ ~s.environmentUsed => s.reverseCount=s.reverseAtComplete
NoReplaceCollisionPreservesEndpoints == s.blockedLedger#0 => <<At(s.blockedLedger,"target"),At(s.blockedLedger,"staged")>>=s.blockedPair
UnchangedUntouched == s.started => \A k \in (K \ Changed(s.case)) \ s.affected:At(k,"target")=O(k) /\ At(k,"staged")=Nil
UnselectedRootsUntouched == \A r \in Roots \ Selected(s.case):s.objects[RootLive(r)]=RootOld(r) /\ s.objects[RootStage(r)]=Nil
InvalidNeverStarts == ~Preflight => ~s.started
RejectedUntouched == s.phase="rejected" => s.objects=Initial(s.case) /\ ~s.pending /\ ~s.started /\ s.createCalls=0 /\ s.freezeCalls=0 /\ s.published={} /\ s.reversed={}
EventuallyTerminal == <>(s.phase \in {"done","rolled-back","unknown","rejected"})
WindowRaw(direction,kind,side,k) == CASE
 direction="forward" /\ kind="existing" /\ side="target" -> At(k,"target")=N(k) /\ At(k,"staged")=F(s.locations[<<k,"target">>])
 [] direction="forward" /\ kind="existing" /\ side="staged" -> At(k,"target")=F(s.locations[<<k,"staged">>]) /\ At(k,"staged")=O(k)
 [] direction="reverse" /\ kind="existing" /\ side="target" -> At(k,"target")=O(k) /\ At(k,"staged")=F(s.locations[<<k,"target">>])
 [] direction="reverse" /\ kind="existing" /\ side="staged" -> At(k,"target")=F(s.locations[<<k,"staged">>]) /\ At(k,"staged")=N(k)
 [] direction="forward" /\ kind="missing" /\ side="target" -> At(k,"target")=F(s.locations[<<k,"target">>]) /\ At(k,"staged")=N(k) /\ s.blockedLedger=k
 [] direction="forward" /\ kind="missing" /\ side="staged" -> At(k,"target")=F(s.locations[<<k,"staged">>]) /\ At(k,"staged")=Nil
 [] direction="reverse" /\ kind="missing" /\ side="target" -> At(k,"target")=Nil /\ At(k,"staged")=F(s.locations[<<k,"target">>])
 [] OTHER -> At(k,"target")=N(k) /\ At(k,"staged")=F(s.locations[<<k,"staged">>]) /\ s.blockedLedger=k
WindowReached(direction,kind,side) == s.phase="unknown" /\ s.windowExecuted /\ s.windowLedger#0 /\ s.window=direction \o "-" \o kind \o "-" \o side /\ WindowRaw(direction,kind,side,s.windowLedger)
NoForwardExistingTarget == ~WindowReached("forward","existing","target")
NoForwardExistingStage == ~WindowReached("forward","existing","staged")
NoForwardMissingTarget == ~WindowReached("forward","missing","target")
NoForwardMissingStage == ~WindowReached("forward","missing","staged")
NoReverseExistingTarget == ~WindowReached("reverse","existing","target")
NoReverseExistingStage == ~WindowReached("reverse","existing","staged")
NoReverseMissingTarget == ~WindowReached("reverse","missing","target")
NoReverseMissingStage == ~WindowReached("reverse","missing","staged")
NoActualRollbackClearGap == ~(s.phase="rolled-back" /\ s.clearBeforeCrash /\ s.postReverseCrashes>=3 /\ Cardinality(s.published)>=3 /\ s.reversed=s.published /\ s.reversed \cap Missing(s.case)#{})
NoTwoCrashesAfterReverse == ~(s.phase="rolled-back" /\ s.postReverseCrashes>=2 /\ Cardinality(s.reversed)>=3 /\ s.reversed \cap Missing(s.case)#{} /\ s.reversed \ Missing(s.case)#{})
NoCompleteAfterClearGap == ~(s.phase="done" /\ s.case=1 /\ s.published=K /\ s.clearAfterCrash)
NoRetainedDeleteBeforeClear == ~(s.phase="unknown" /\ s.deleteBeforeClear)
NoRetainedDeleteAfterClear == ~(s.phase="unknown" /\ s.deleteAfterClear)
NoEvidenceLossBlocked == ~(s.phase="unknown" /\ s.evidenceDeleted)
NoPathCollisionRejected == ~(s.phase="rejected" /\ s.case=4)
NoIdentityAliasRejected == ~(s.phase="rejected" /\ s.case=5)
NoCrossVolumeRejected == ~(s.phase="rejected" /\ s.case=6)
NoBudgetRejected == ~(s.phase="rejected" /\ s.case=7)
NoParentRejected == ~(s.phase="rejected" /\ s.case=8)
NoRootOverlapRejected == ~(s.phase="rejected" /\ s.case=9)
=============================================================================
