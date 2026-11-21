------------------------ MODULE Cache ------------------------
EXTENDS TLC
VARIABLES phase, slot, foreignEver, pointer, crashed, owned, priorOwned, created, durable
vars == <<phase,slot,foreignEver,pointer,crashed,owned,priorOwned,created,durable>>
Init == /\ phase = "querying" /\ slot \in {"empty","ours","foreign"} /\ foreignEver = (slot = "foreign") /\ pointer = FALSE /\ crashed = FALSE /\ priorOwned \in BOOLEAN /\ owned = (slot = "ours" /\ priorOwned) /\ created = FALSE /\ durable = (slot = "ours")
QueryDone == /\ phase = "querying" /\ phase' = "publishing" /\ UNCHANGED <<slot,foreignEver,pointer,crashed,owned,priorOwned,created,durable>>
ForeignArrives == /\ phase \in {"querying","publishing","blob-ready","durable"} /\ ~foreignEver /\ slot' = "foreign" /\ foreignEver' = TRUE /\ UNCHANGED <<phase,pointer,crashed,owned,priorOwned,created,durable>>
CreateNew == /\ phase = "publishing" /\ slot = "empty" /\ slot' = "ours" /\ created' = TRUE /\ durable' = FALSE /\ phase' = "blob-ready" /\ UNCHANGED <<foreignEver,pointer,crashed,owned,priorOwned>>
ReuseIdentical == /\ phase = "publishing" /\ slot = "ours" /\ phase' = "blob-ready" /\ UNCHANGED <<slot,foreignEver,pointer,crashed,owned,priorOwned,created,durable>>
RejectForeign == /\ phase \in {"publishing","blob-ready","durable"} /\ slot = "foreign" /\ phase' = "review" /\ UNCHANGED <<slot,foreignEver,pointer,crashed,owned,priorOwned,created,durable>>
SyncDirectory == /\ phase = "blob-ready" /\ slot = "ours" /\ durable' = TRUE /\ phase' = "durable" /\ UNCHANGED <<slot,foreignEver,pointer,crashed,owned,priorOwned,created>>
CommitReference == /\ phase = "durable" /\ slot = "ours" /\ durable /\ pointer' = TRUE /\ owned' = (owned \/ created) /\ phase' = "done" /\ UNCHANGED <<slot,foreignEver,crashed,priorOwned,created,durable>>
Crash == /\ phase \in {"querying","publishing","blob-ready","durable"} /\ ~crashed /\ crashed' = TRUE /\ phase' = "interrupted" /\ UNCHANGED <<slot,foreignEver,pointer,owned,priorOwned,created,durable>>
Terminal == phase \in {"review","done","interrupted"}
Done == /\ Terminal /\ UNCHANGED vars
Advance == QueryDone \/ CreateNew \/ ReuseIdentical \/ RejectForeign \/ SyncDirectory \/ CommitReference
Next == Advance \/ ForeignArrives \/ Crash \/ Done
Spec == Init /\ [][Next]_vars /\ WF_vars(Advance)
NeverOverwriteForeign == foreignEver => slot = "foreign"
ReferenceOnlyPublishedBlob == pointer => (slot = "ours" /\ durable)
NoOwnershipFromReuse == owned => (priorOwned \/ created)
NeverNewOwned == ~(phase = "done" /\ created /\ owned)
NeverBorrowedDone == ~(phase = "done" /\ slot = "ours" /\ ~priorOwned /\ ~created /\ ~owned)
EventuallyReport == <>Terminal
===============================================================
