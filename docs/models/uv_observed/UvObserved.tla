------------------------- MODULE UvObserved -------------------------
EXTENDS Naturals, FiniteSets, TLC
VARIABLES phase, request, dry, owners, initialOwners, frozenOwners, selectedOwner,
          resolveConflict, ownerChanged, commitOwners, receiptOwner,
          root, stamp, frozen, observation, journalOp, receiptOp,
          nativeRuns, stateWrites, macopsBChanges, recovered, lock
vars == <<phase,request,dry,owners,initialOwners,frozenOwners,selectedOwner,
          resolveConflict,ownerChanged,commitOwners,receiptOwner,root,stamp,frozen,
          observation,journalOp,receiptOp,nativeRuns,stateWrites,macopsBChanges,recovered,lock>>
OwnerSets == {{},{"alice"},{"bob"},{"alice","bob"}}
SelectOwner(set) == IF Cardinality(set)=1 THEN CHOOSE one \in set: TRUE ELSE "generated"
Identity(r,n) == <<"uv", r, "demo-tool">>
Init == /\ phase="resolve" /\ request \in {"read","adopt","write"} /\ dry \in BOOLEAN
 /\ owners \in OwnerSets /\ initialOwners=owners /\ frozenOwners={} /\ selectedOwner="none"
 /\ resolveConflict=FALSE /\ ownerChanged=FALSE /\ commitOwners={} /\ receiptOwner="none"
 /\ root \in {"A","B"} /\ stamp=0 /\ frozen=0 /\ observation \in {"present","missing","unknown"}
 /\ journalOp=0 /\ receiptOp=0 /\ nativeRuns=0 /\ stateWrites=0 /\ macopsBChanges=0 /\ recovered=FALSE /\ lock=FALSE
Resolve == /\ phase="resolve"
 /\ phase'=(IF Cardinality(owners)>1 \/ request="write" THEN "blocked" ELSE "planned")
 /\ frozenOwners'=owners /\ selectedOwner'=SelectOwner(owners) /\ resolveConflict'=(Cardinality(owners)>1)
 /\ UNCHANGED <<request,dry,owners,initialOwners,ownerChanged,commitOwners,receiptOwner,root,stamp,frozen,observation,journalOp,receiptOp,nativeRuns,stateWrites,macopsBChanges,recovered,lock>>
Preview == /\ phase="planned" /\ (dry \/ request="read") /\ phase'="done"
 /\ UNCHANGED <<request,dry,owners,initialOwners,frozenOwners,selectedOwner,resolveConflict,ownerChanged,commitOwners,receiptOwner,root,stamp,frozen,observation,journalOp,receiptOp,nativeRuns,stateWrites,macopsBChanges,recovered,lock>>
Prepare == /\ phase="planned" /\ ~dry /\ request="adopt"
 /\ phase'=(IF observation="present" /\ stamp=frozen /\ owners=frozenOwners THEN "prepared" ELSE "unknown")
 /\ lock'=(phase'="prepared") /\ journalOp'=(IF phase'="prepared" THEN 1 ELSE 0) /\ stateWrites'=(IF phase'="prepared" THEN 1 ELSE 0)
 /\ UNCHANGED <<request,dry,owners,initialOwners,frozenOwners,selectedOwner,resolveConflict,ownerChanged,commitOwners,receiptOwner,root,stamp,frozen,observation,receiptOp,nativeRuns,macopsBChanges,recovered>>
Commit == /\ phase="prepared"
 /\ phase'=(IF stamp=frozen /\ observation="present" /\ Cardinality(owners)<=1 /\ owners=frozenOwners THEN "committed" ELSE "unknown")
 /\ receiptOp'=(IF phase'="committed" THEN journalOp ELSE receiptOp)
 /\ receiptOwner'=(IF phase'="committed" THEN selectedOwner ELSE receiptOwner)
 /\ commitOwners'=(IF phase'="committed" THEN owners ELSE commitOwners)
 /\ owners'=(IF phase'="committed" THEN {selectedOwner} ELSE owners)
 /\ UNCHANGED <<request,dry,initialOwners,frozenOwners,selectedOwner,resolveConflict,ownerChanged,root,stamp,frozen,observation,journalOp,nativeRuns,stateWrites,macopsBChanges,recovered,lock>>
Crash == /\ phase \in {"prepared","committed"} /\ phase'="crashed" /\ lock'=FALSE
 /\ UNCHANGED <<request,dry,owners,initialOwners,frozenOwners,selectedOwner,resolveConflict,ownerChanged,commitOwners,receiptOwner,root,stamp,frozen,observation,journalOp,receiptOp,nativeRuns,stateWrites,macopsBChanges,recovered>>
Recover == /\ phase="crashed" /\ recovered'=TRUE
 /\ phase'=(IF receiptOp=journalOp /\ receiptOp=1 /\ stamp=frozen /\ owners={selectedOwner} /\ receiptOwner=selectedOwner THEN "done" ELSE "unknown")
 /\ UNCHANGED <<request,dry,owners,initialOwners,frozenOwners,selectedOwner,resolveConflict,ownerChanged,commitOwners,receiptOwner,root,stamp,frozen,observation,journalOp,receiptOp,nativeRuns,stateWrites,macopsBChanges,lock>>
Finish == /\ phase="committed" /\ phase'="done"
 /\ UNCHANGED <<request,dry,owners,initialOwners,frozenOwners,selectedOwner,resolveConflict,ownerChanged,commitOwners,receiptOwner,root,stamp,frozen,observation,journalOp,receiptOp,nativeRuns,stateWrites,macopsBChanges,recovered,lock>>
External == /\ phase \in {"planned","prepared"} /\ stamp=0 /\ stamp'=1
 /\ UNCHANGED <<phase,request,dry,owners,initialOwners,frozenOwners,selectedOwner,resolveConflict,ownerChanged,commitOwners,receiptOwner,root,frozen,observation,journalOp,receiptOp,nativeRuns,stateWrites,macopsBChanges,recovered,lock>>
LateOwner == /\ phase \in {"planned","prepared"} /\ ~ownerChanged
 /\ owners' \in OwnerSets \ {owners} /\ ownerChanged'=TRUE
 /\ UNCHANGED <<phase,request,dry,initialOwners,frozenOwners,selectedOwner,resolveConflict,commitOwners,receiptOwner,root,stamp,frozen,observation,journalOp,receiptOp,nativeRuns,stateWrites,macopsBChanges,recovered,lock>>
Next == Resolve \/ Preview \/ Prepare \/ Commit \/ Crash \/ Recover \/ Finish \/ External \/ LateOwner
Spec == Init /\ [][Next]_vars
UvNativeRunsAlwaysZero == nativeRuns=0
ReadOnlyNoStateWrites == request="read" => stateWrites=0
DryRunNoStateWrites == dry => stateWrites=0
NoSecondOwner == receiptOp=1 => Cardinality(owners)=1
ConflictNeverProceeds == resolveConflict => phase="blocked"
FrozenOwnerCAS == receiptOp=1 => /\ commitOwners=frozenOwners /\ receiptOwner=SelectOwner(frozenOwners) /\ receiptOwner=selectedOwner /\ owners={receiptOwner}
LateOwnerRefusesReceipt == ownerChanged => receiptOp=0
SingletonReplacementCAS == ~(receiptOp=1 /\ initialOwners={"alice"} /\ ownerChanged /\ commitOwners={"bob"})
NoImplicitWriteGrant == receiptOp=1 => nativeRuns=0
SameCanonicalIdentityAcrossViews == Identity(root,"Demo_Tool")=Identity(root,"demo-tool") /\ Identity("A","demo-tool")#Identity("B","demo-tool")
AdoptOnlyStablePresent == phase="committed" => observation="present" /\ stamp=frozen
DriftBeforeCommitBlocksReceipt == stamp#frozen => receiptOp=0
CommitUsesOriginalOperation == receiptOp=1 => journalOp=receiptOp
RecoverNeverSpawns == recovered => nativeRuns=0
OtherRootUnchangedByMacops == macopsBChanges=0
ExclusiveCommit == phase="committed" => lock
NoAdoptSuccess == ~(phase="done" /\ request="adopt" /\ receiptOp=1)
NoCrashRecovery == ~(phase="done" /\ recovered /\ receiptOp=1)
NoAliceReuse == ~(phase="committed" /\ initialOwners={"alice"} /\ frozenOwners=initialOwners /\ selectedOwner="alice" /\ receiptOwner="alice" /\ ~ownerChanged)
NoBobReuse == ~(phase="committed" /\ initialOwners={"bob"} /\ frozenOwners=initialOwners /\ selectedOwner="bob" /\ receiptOwner="bob" /\ ~ownerChanged)
NoCASUnknown == ~(phase="unknown" /\ stamp#frozen /\ journalOp=1 /\ receiptOp=0)
NoLateOwnerUnknown == ~(phase="unknown" /\ ownerChanged /\ stamp=frozen /\ journalOp=1 /\ receiptOp=0)
NoLateSingletonUnknown == ~(phase="unknown" /\ initialOwners={"alice"} /\ frozenOwners={"alice"} /\ owners={"bob"} /\ ownerChanged /\ stamp=frozen /\ journalOp=1 /\ receiptOp=0)
NoDoubleOwnerBlocked == ~(phase="blocked" /\ Cardinality(owners)>1)
NoReadSuccess == ~(phase="done" /\ request="read")
=============================================================================
