-------------------------- MODULE NativeRunnerLock --------------------------
EXTENDS Naturals, FiniteSets, TLC
CONSTANTS Roots, HomeMatches, GateValid, QueryRequired
VARIABLES phase, parents, workers, parentFD, workerFD, locked, gate,
          accepted, checked, beforeDone, nativeLive, queryLive, afterObserved,
          calls, nativeFD, queryFD, unknownSeen
vars == <<phase,parents,workers,parentFD,workerFD,locked,gate,accepted,checked,
          beforeDone,nativeLive,queryLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
Final == {"exited","refused","blocked"}
Init == /\ phase=[r \in Roots |-> "idle"] /\ parents={} /\ workers={}
        /\ parentFD={} /\ workerFD={} /\ locked={} /\ gate={} /\ accepted={}
        /\ checked={} /\ beforeDone=(IF QueryRequired THEN {} ELSE Roots)
        /\ nativeLive={} /\ queryLive={} /\ afterObserved={}
        /\ calls=[r \in Roots |-> 0] /\ nativeFD={} /\ queryFD={} /\ unknownSeen={}
Start(r) == /\ phase[r] \in {"idle","blocked"} /\ locked={} /\ calls[r]=0
            /\ phase'=[phase EXCEPT ![r]="parent-held"]
            /\ parents'=parents \cup {r} /\ parentFD'=parentFD \cup {r}
            /\ locked'=locked \cup {r}
            /\ UNCHANGED <<workers,workerFD,gate,accepted,checked,beforeDone,
                 nativeLive,queryLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
Blocked(r) == /\ phase[r]="idle" /\ locked#{}
              /\ phase'=[phase EXCEPT ![r]="blocked"]
              /\ UNCHANGED <<parents,workers,parentFD,workerFD,locked,gate,accepted,
                   checked,beforeDone,nativeLive,queryLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
Spawn(r) == /\ phase[r]="parent-held" /\ r \in parents
            /\ phase'=[phase EXCEPT ![r]="waiting"]
            /\ workers'=workers \cup {r} /\ workerFD'=workerFD \cup {r}
            /\ UNCHANGED <<parents,parentFD,locked,gate,accepted,checked,beforeDone,
                 nativeLive,queryLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
CheckHome(r) == /\ phase[r]="waiting" /\ r \notin checked
                /\ phase'=[phase EXCEPT ![r]=(IF HomeMatches /\ r \in workerFD THEN "waiting" ELSE "refused")]
                /\ checked'=(IF HomeMatches /\ r \in workerFD THEN checked \cup {r} ELSE checked)
                /\ UNCHANGED <<parents,workers,parentFD,workerFD,locked,gate,accepted,beforeDone,
                     nativeLive,queryLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
SendGate(r) == /\ phase[r]="waiting" /\ r \in parents /\ r \notin gate
               /\ gate'=gate \cup {r}
               /\ UNCHANGED <<phase,parents,workers,parentFD,workerFD,locked,accepted,checked,beforeDone,
                    nativeLive,queryLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
AcceptGate(r) == /\ phase[r]="waiting" /\ r \in gate /\ r \notin accepted
                 /\ phase'=[phase EXCEPT ![r]=(IF GateValid THEN "waiting" ELSE "refused")]
                 /\ accepted'=(IF GateValid THEN accepted \cup {r} ELSE accepted)
                 /\ UNCHANGED <<parents,workers,parentFD,workerFD,locked,gate,checked,beforeDone,
                      nativeLive,queryLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
BeforeQuery(r) == /\ phase[r]="waiting" /\ r \in accepted /\ r \in checked /\ r \notin beforeDone
                  /\ phase'=[phase EXCEPT ![r]="query"] /\ queryLive'=queryLive \cup {r}
                  /\ UNCHANGED <<parents,workers,parentFD,workerFD,locked,gate,accepted,checked,beforeDone,
                       nativeLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
QueryDone(r) == /\ phase[r]="query" /\ phase'=[phase EXCEPT ![r]="waiting"]
                /\ queryLive'=queryLive \ {r} /\ beforeDone'=beforeDone \cup {r}
                /\ UNCHANGED <<parents,workers,parentFD,workerFD,locked,gate,accepted,checked,
                     nativeLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
NativeStart(r) == /\ phase[r]="waiting" /\ r \in checked /\ r \in accepted /\ r \in beforeDone
                  /\ calls[r]=0
                  /\ phase'=[phase EXCEPT ![r]="native"] /\ calls'=[calls EXCEPT ![r]=@+1]
                  /\ nativeLive'=nativeLive \cup {r}
                  /\ UNCHANGED <<parents,workers,parentFD,workerFD,locked,gate,accepted,checked,
                       beforeDone,queryLive,afterObserved,nativeFD,queryFD,unknownSeen>>
NativeDone(r) == /\ r \in nativeLive /\ phase[r] \in {"native","waiting"}
                 /\ phase'=[phase EXCEPT ![r]="native-done"] /\ nativeLive'=nativeLive \ {r}
                 /\ UNCHANGED <<parents,workers,parentFD,workerFD,locked,gate,accepted,checked,
                      beforeDone,queryLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
AfterStart(r) == /\ phase[r]="native-done"
                 /\ phase'=[phase EXCEPT ![r]="after"] /\ queryLive'=queryLive \cup {r}
                 /\ UNCHANGED <<parents,workers,parentFD,workerFD,locked,gate,accepted,checked,beforeDone,
                      nativeLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
AfterDone(r) == /\ phase[r]="after" /\ phase'=[phase EXCEPT ![r]="after-done"]
                /\ queryLive'=queryLive \ {r} /\ afterObserved'=afterObserved \cup {r}
                /\ UNCHANGED <<parents,workers,parentFD,workerFD,locked,gate,accepted,checked,beforeDone,
                     nativeLive,calls,nativeFD,queryFD,unknownSeen>>
ExitWorker(r) == /\ r \in workers
                 /\ ((phase[r]="after-done" /\ r \in afterObserved) \/ phase[r]="refused")
                 /\ phase'=[phase EXCEPT ![r]="exited"] /\ workers'=workers \ {r}
                 /\ workerFD'=workerFD \ {r}
                 /\ locked'=(IF r \in parentFD THEN locked ELSE locked \ {r})
                 /\ UNCHANGED <<parents,parentFD,gate,accepted,checked,beforeDone,nativeLive,
                      queryLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
Abandon(r) == /\ phase[r] \in {"parent-held","waiting"} /\ r \notin parents /\ r \notin gate
              /\ phase'=[phase EXCEPT ![r]="exited"] /\ workers'=workers \ {r}
              /\ workerFD'=workerFD \ {r} /\ locked'=locked \ {r}
              /\ UNCHANGED <<parents,parentFD,gate,accepted,checked,beforeDone,nativeLive,
                   queryLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
ParentExit(r) == /\ phase[r] \in Final /\ r \in parents /\ r \notin workers
                 /\ parents'=parents \ {r} /\ parentFD'=parentFD \ {r}
                 /\ locked'=(IF r \in workerFD THEN locked ELSE locked \ {r})
                 /\ UNCHANGED <<phase,workers,workerFD,gate,accepted,checked,beforeDone,nativeLive,
                      queryLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
KillParent(r) == /\ r \in parents /\ parents'=parents \ {r} /\ parentFD'=parentFD \ {r}
                 /\ locked'=(IF r \in workerFD THEN locked ELSE locked \ {r})
                 /\ UNCHANGED <<phase,workers,workerFD,gate,accepted,checked,beforeDone,nativeLive,
                      queryLive,afterObserved,calls,nativeFD,queryFD,unknownSeen>>
Unknown(r) == /\ r \in nativeLive /\ r \notin parents /\ r \notin unknownSeen
              /\ phase'=[phase EXCEPT ![r]="waiting"] /\ unknownSeen'=unknownSeen \cup {r}
              /\ UNCHANGED <<parents,workers,parentFD,workerFD,locked,gate,accepted,checked,beforeDone,
                   nativeLive,queryLive,afterObserved,calls,nativeFD,queryFD>>
Done == /\ \A r \in Roots: phase[r] \in Final /\ parents={} /\ workers={}
        /\ UNCHANGED vars
Progress == (\E r \in Roots: Start(r) \/ Blocked(r) \/ Spawn(r) \/ CheckHome(r) \/ SendGate(r)
             \/ AcceptGate(r) \/ BeforeQuery(r) \/ QueryDone(r) \/ NativeStart(r) \/ NativeDone(r)
             \/ AfterStart(r) \/ AfterDone(r) \/ ExitWorker(r) \/ Abandon(r) \/ ParentExit(r)) \/ Done
Next == Progress \/ (\E r \in Roots: KillParent(r) \/ Unknown(r))
Spec == Init /\ [][Next]_vars /\ WF_vars(Progress)
TypeOK == /\ parentFD \subseteq Roots /\ workerFD \subseteq Roots /\ locked \subseteq Roots
          /\ nativeLive \subseteq Roots /\ queryLive \subseteq Roots /\ calls \in [Roots -> Nat]
NoParallelNative == Cardinality(nativeLive)<=1
NoCrossStateDelivery == Cardinality(nativeLive \cup queryLive)<=1
NoUngatedNative == \A r \in Roots: calls[r]>0 => r \in gate /\ GateValid
NoWrongHomeNative == ~HomeMatches => \A r \in Roots: calls[r]=0
NoInheritedLock == nativeFD={} /\ queryFD={}
FlockMatchesReferences == locked=parentFD \cup workerFD
DeliveryKeepsAuthority == nativeLive \cup queryLive \subseteq workerFD
WorkerAfterBeforeExit == \A r \in Roots: phase[r]="exited" /\ calls[r]>0 => r \in afterObserved
OneOriginalNative == \A r \in Roots: calls[r]<=1
NoRepeat == [][\A r \in Roots: calls[r]=0 \/ calls'[r]=calls[r]]_vars
EventuallyTerminal == <> (\A r \in Roots: phase[r] \in Final /\ parents={} /\ workers={})
NoKilledParentNative == ~(\E r \in Roots: r \in nativeLive /\ r \notin parents)
NoKilledParentAfter == ~(\E r \in Roots: phase[r]="after" /\ r \notin parents)
NoUngatedExit == ~(\E r \in Roots: phase[r]="exited" /\ r \notin gate /\ calls[r]=0)
NoBothComplete == ~(\A r \in Roots: r \in afterObserved /\ calls[r]=1)
=============================================================================
