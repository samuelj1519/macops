-------------------------- MODULE BrewNative --------------------------
EXTENDS Naturals, FiniteSets, TLC
VARIABLE s
vars == <<s>>
Bindings == {"original", "program-changed", "cellar-changed", "env-changed"}
Names == {"foo", "vendor/tap/foo", "foo@1"}
Prefixes == {"A", "B"}
Owner(prefix, fullName, binding) == <<"formula", prefix, fullName>>
Context(prefix, binding) == <<"brew", prefix>>
Key == Owner("A", "foo", "original")
Selected(case) == IF case="wrong-scope" THEN Owner("B", "vendor/tap/foo", "original") ELSE Key
Valid(case) == case \in {"good", "missing"}
Terminal == {"done", "blocked"}
Outstanding == {"prepared", "running", "unknown"}
Init == s = [phase |-> "candidate", request |-> "maintain", trusted |-> FALSE,
 queryBinding |-> "none", binding |-> "original", frozen |-> "original", opBinding |-> "none", continuity |-> FALSE,
 owners |-> 1, overlap |-> FALSE, case |-> "none", selected |-> Key,
 runs |-> 0, queries |-> 0, operation |-> 0, receiptOp |-> 0,
 owner |-> Key, committed |-> FALSE, recovered |-> FALSE, post |-> "none",
 execTrusted |-> FALSE, execBinding |-> "none", execOwners |-> 0,
 execOverlap |-> FALSE, execKey |-> Key, event |-> "init"]
PermitQuery == /\ s.phase="candidate"
 /\ s'=[s EXCEPT !.phase="query-allowed", !.trusted=TRUE, !.event="permit-query"]
Preview == /\ s.phase="candidate"
 /\ s'=[s EXCEPT !.phase="done", !.request="dry", !.event="preview"]
Query(case) == /\ s.phase="query-allowed"
 /\ case \in {"good", "missing", "unknown", "incomplete", "wrong-scope", "policy-disabled"}
 /\ s'=[s EXCEPT !.phase="queried", !.case=case, !.selected=Selected(case),
                    !.queries=@+1, !.queryBinding=s.binding, !.event="query"]
Bind == /\ s.phase="queried"
 /\ s'=[s EXCEPT !.phase=IF Valid(s.case) /\ s.selected=Key THEN "bound" ELSE "blocked",
                    !.frozen=s.binding, !.owner=Owner("A", "foo", s.binding), !.event="bind"]
Baseline == /\ s.phase="bound" /\ s.case="good"
 /\ s'=[s EXCEPT !.phase="done", !.request="adopt", !.event="baseline"]
Noop == /\ s.phase="bound" /\ s.case="missing"
 /\ LET ok == s.binding=s.frozen /\ s.owners=1 /\ ~s.overlap
    IN s'=[s EXCEPT !.phase=IF ok THEN "done" ELSE "blocked", !.post="complete", !.committed=ok, !.event="noop"]
Prepare == /\ s.phase="bound" /\ s.case="good"
 /\ s'=[s EXCEPT !.phase="prepared", !.operation=1, !.opBinding=s.frozen, !.event="prepare"]
Drift(binding) == /\ s.phase \in {"bound", "prepared", "running"} /\ s.binding="original"
 /\ binding \in Bindings \ {"original"}
 /\ s'=[s EXCEPT !.binding=binding, !.event="binding-drift"]
Conflict == /\ s.phase \in {"bound", "prepared", "running"} /\ s.owners=1
 /\ s'=[s EXCEPT !.owners=2, !.event="owner-conflict"]
Overlap == /\ s.phase \in {"bound", "prepared", "running"} /\ ~s.overlap
 /\ s'=[s EXCEPT !.overlap=TRUE, !.event="cellar-overlap"]
ProveContinuity == /\ s.phase="bound" /\ s.binding="cellar-changed" /\ ~s.continuity
 /\ s'=[s EXCEPT !.continuity=TRUE, !.event="prove-continuity"]
Rebind == /\ s.phase="bound" /\ s.binding#s.frozen /\ s.owners=1 /\ ~s.overlap
 /\ (s.binding#"cellar-changed" \/ s.continuity)
 /\ s'=[s EXCEPT !.frozen=s.binding, !.owner=Owner("A", "foo", s.binding), !.event="rebind"]
Worker == /\ s.phase="prepared"
 /\ IF s.trusted /\ s.binding=s.frozen /\ s.owners=1 /\ ~s.overlap
          /\ s.case="good" /\ s.selected=Key /\ s.runs=0
    THEN s'=[s EXCEPT !.phase="running", !.runs=@+1, !.execTrusted=s.trusted,
                         !.execBinding=s.binding, !.execOwners=s.owners,
                         !.execOverlap=s.overlap, !.execKey=s.selected, !.event="native-start"]
    ELSE s'=[s EXCEPT !.phase="blocked", !.event="worker-reject"]
Finish(post) == /\ s.phase="running" /\ post \in {"complete", "incomplete", "unknown"}
 /\ LET ok == post="complete" /\ s.binding=s.frozen /\ s.owners=1 /\ ~s.overlap
    IN s'=[s EXCEPT !.phase=IF ok THEN "done" ELSE "unknown", !.post=post,
                       !.committed=ok, !.receiptOp=IF ok THEN s.operation ELSE 0, !.event="finish"]
Crash == /\ s.phase="running"
 /\ s'=[s EXCEPT !.phase="unknown", !.post="unknown", !.event="crash"]
Recover(post) == /\ s.phase="unknown" /\ post \in {"complete", "incomplete", "unknown"}
 /\ LET ok == post="complete" /\ s.binding=s.frozen /\ s.owners=1 /\ ~s.overlap
    IN s'=[s EXCEPT !.phase=IF ok THEN "done" ELSE "blocked", !.post=post,
                       !.committed=ok, !.receiptOp=IF ok THEN s.operation ELSE 0,
                       !.queries=@+1, !.recovered=TRUE, !.event="recover-query"]
Done == /\ s.phase \in Terminal /\ UNCHANGED s
Next == PermitQuery \/ Preview \/ (\E case \in {"good", "missing", "unknown", "incomplete", "wrong-scope", "policy-disabled"}: Query(case))
 \/ Bind \/ Baseline \/ Noop \/ Prepare \/ (\E b \in Bindings: Drift(b))
 \/ Conflict \/ Overlap \/ ProveContinuity \/ Rebind \/ Worker
 \/ (\E p \in {"complete", "incomplete", "unknown"}: Finish(p)) \/ Crash
 \/ (\E p \in {"complete", "incomplete", "unknown"}: Recover(p)) \/ Done
Spec == Init /\ [][Next]_vars /\ WF_vars(Next)
TypeOK == /\ s.phase \in {"candidate", "query-allowed", "queried", "bound", "prepared", "running", "unknown", "done", "blocked"}
 /\ s.binding \in Bindings /\ s.frozen \in Bindings /\ s.runs \in Nat /\ s.queries \in Nat
 /\ s.owners \in {1,2} /\ s.operation \in {0,1} /\ s.receiptOp \in {0,1}
StableOwner == s.owner=Key /\ Owner("A", "foo", s.binding)=Key
StableContext == Context("A", s.binding)=Context("A", "original")
DistinctOwners == \A p1,p2 \in Prefixes, n1,n2 \in Names:
 (Owner(p1,n1,"original")=Owner(p2,n2,"original")) <=> (p1=p2 /\ n1=n2)
NoUntrustedQuery == s.queries>0 => s.trusted
PreviewNoNative == s.request="dry" => s.queries=0 /\ s.runs=0 /\ ~s.committed
BaselineNoNative == s.request="adopt" => s.runs=0
NativeCount == s.runs<=1
NativeGate == s.runs>0 => s.execTrusted /\ s.execBinding=s.opBinding /\ s.execOwners=1 /\ ~s.execOverlap
SelectedScope == s.runs>0 => s.execKey=Key
NoRetarget == s.operation=1 => s.frozen=s.opBinding /\ s.owner=Key
TypedCommit == s.committed => s.post="complete" /\ s.binding=s.frozen /\ s.owners=1 /\ ~s.overlap
OriginalOperation == s.receiptOp#0 => s.receiptOp=s.operation
NoRepeatStep == s.runs>0 => s'.runs=s.runs
NoRepeat == [][NoRepeatStep]_vars
RecoveryNoNativeStep == s.phase="unknown" => s'.runs=s.runs
RecoveryNoNative == [][RecoveryNoNativeStep]_vars
EventuallyTerminal == <>(s.phase \in Terminal)
NoWriterSuccess == ~(s.phase="done" /\ s.runs=1 /\ ~s.recovered)
NoRecoverySuccess == ~(s.phase="done" /\ s.recovered /\ s.runs=1)
NoRebindSuccess == ~(s.phase="done" /\ s.runs=1 /\ s.frozen#"original")
NoCellarRebind == ~(s.phase="done" /\ s.runs=1 /\ s.frozen="cellar-changed" /\ s.continuity)
NoLateDriftUnknown == ~(s.phase="unknown" /\ s.runs=1 /\ s.binding#s.frozen)
NoConflictReject == ~(s.phase="blocked" /\ s.owners=2 /\ s.operation=1 /\ s.runs=0)
NoTypedMissingNoop == ~(s.phase="done" /\ s.case="missing" /\ s.committed /\ s.runs=0)
NoDryPreview == ~(s.phase="done" /\ s.request="dry" /\ s.queries=0)
NoBaseline == ~(s.phase="done" /\ s.request="adopt" /\ s.runs=0)

LiteralOwner == s.owner= <<"formula","A","foo">> /\ Owner("A","foo",s.binding)= <<"formula","A","foo">>
LiteralContext == Context("A",s.binding)= <<"brew","A">>
LiteralRebindStep == s'.event="rebind" => s'.owner=s.owner /\ s'.owners=s.owners /\ s'.operation=s.operation /\ s'.opBinding=s.opBinding
LiteralRebind == [][LiteralRebindStep]_vars
LiteralUnknownStep == s.phase="unknown" => s'.runs=s.runs /\ s'.operation=s.operation /\ s'.opBinding=s.opBinding
LiteralUnknown == [][LiteralUnknownStep]_vars
LiteralRunStep == (s.runs=0 => s'.runs \in {0,1}) /\ (s.runs>0 => s'.runs=s.runs)
LiteralRun == [][LiteralRunStep]_vars
LiteralCellarRebindStep == s'.event="rebind" /\ s.binding="cellar-changed" => s.continuity
LiteralCellarRebind == [][LiteralCellarRebindStep]_vars
LiteralOverlapStartStep == s'.event="native-start" => ~s.overlap
LiteralOverlapStart == [][LiteralOverlapStartStep]_vars

FreshMissing == s.committed /\ s.case="missing" => s.queryBinding=s.binding
=============================================================================
