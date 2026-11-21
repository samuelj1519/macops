-------------------------- MODULE ProfileConfig --------------------------
EXTENDS Naturals, FiniteSets, TLC
CONSTANT Missing, External
VARIABLE s
vars == <<s>>
Owners == {"mi", "zen"}
Values == {Missing, "on", "off"}
None == "no-claim"
EmptyClaims == [o \in Owners |-> None]
HasClaims(c) == \E o \in Owners : c[o] # None
Agree(c,d) == \A o \in Owners : c[o] \in {None,d}
Known == s.current \in {s.original,s.applied}
Consistent(c) == \A a,b \in Owners : c[a] = None \/ c[b] = None \/ c[a] = c[b]
NextClaims(selected,d) == [o \in Owners |-> IF o \in selected THEN d ELSE s.claims[o]]
Init == \E value \in Values, parent \in {"missing","empty","nonempty"}, other \in BOOLEAN, a \in {"on","off"}, b \in {"on","off"} :
    /\ (parent \in {"missing","empty"} => value = Missing /\ ~other)
    /\ (parent = "nonempty" => value # Missing \/ other)
    /\ s = [claims |-> EmptyClaims, desired |-> [o \in Owners |-> IF o = "mi" THEN a ELSE b],
             joined |-> {}, retired |-> {}, denied |-> {}, captured |-> FALSE,
             current |-> value, parent |-> (parent # "missing"), other |-> other,
             original |-> value, originalParent |-> parent, witness |-> value,
             parentWitness |-> parent, applied |-> value, expectedOther |-> other,
             otherEdited |-> FALSE, ownedEdited |-> FALSE, tampered |-> FALSE,
             phase |-> "active", lastSelected |-> {}, lastClaims |-> EmptyClaims,
             sharedSeen |-> FALSE, lastRestore |-> FALSE, updated |-> {}, changedUpdate |-> FALSE, rejectedUpdate |-> FALSE, sharedUpdate |-> FALSE,
             lastRejected |-> FALSE, rejectedSnapshot |-> <<value,EmptyClaims,(parent # "missing"),value,parent>>]
Join(o) ==
    /\ s.phase = "active" /\ o \notin s.joined /\ o \notin s.denied
    /\ (~s.captured \/ Known) /\ Agree(s.claims,s.desired[o])
    /\ s' = [s EXCEPT !.claims[o] = s.desired[o], !.joined = @ \cup {o},
             !.original = IF HasClaims(s.claims) THEN @ ELSE s.current,
             !.witness = IF HasClaims(s.claims) THEN @ ELSE s.current,
             !.originalParent = IF HasClaims(s.claims) THEN @ ELSE
                 IF ~s.parent THEN "missing" ELSE IF s.current = Missing /\ ~s.other THEN "empty" ELSE "nonempty",
             !.parentWitness = IF HasClaims(s.claims) THEN @ ELSE
                 IF ~s.parent THEN "missing" ELSE IF s.current = Missing /\ ~s.other THEN "empty" ELSE "nonempty",
             !.captured = TRUE, !.current = s.desired[o], !.applied = s.desired[o], !.parent = TRUE,
             !.sharedSeen = @ \/ HasClaims(s.claims), !.lastRestore = FALSE, !.lastSelected = {}, !.lastRejected = FALSE]
Conflict(o) ==
    /\ s.phase = "active" /\ o \notin s.joined /\ o \notin s.denied /\ ~Agree(s.claims,s.desired[o])
    /\ s' = [s EXCEPT !.denied = @ \cup {o}, !.lastSelected = {}, !.lastRestore = FALSE, !.lastRejected = FALSE]
Retire(o) ==
    /\ s.phase = "active" /\ s.claims[o] # None /\ Known
    /\ LET next == [s.claims EXCEPT ![o] = None] IN
       s' = [s EXCEPT !.claims = next, !.retired = @ \cup {o}, !.lastSelected = {o}, !.lastClaims = s.claims,
             !.current = IF HasClaims(next) THEN s.applied ELSE s.original,
             !.parent = IF HasClaims(next) THEN TRUE ELSE (s.originalParent # "missing" \/ s.original # Missing \/ s.other),
             !.lastRestore = ~HasClaims(next), !.lastRejected = FALSE]
Reconfigure(selected,d) ==
    /\ s.phase = "active" /\ selected # {} /\ selected \subseteq {o \in Owners : s.claims[o] # None}
    /\ selected \cap s.updated = {} /\ Known /\ Consistent(NextClaims(selected,d))
    /\ s' = [s EXCEPT !.claims = NextClaims(selected,d), !.current = d, !.applied = d, !.parent = TRUE,
             !.desired = [o \in Owners |-> IF o \in selected THEN d ELSE s.desired[o]],
             !.updated = @ \cup selected, !.changedUpdate = @ \/ (d # s.applied),
             !.lastSelected = selected, !.lastClaims = s.claims, !.lastRestore = FALSE, !.lastRejected = FALSE,
             !.sharedUpdate = @ \/ (selected = Owners /\ d # s.applied)]
RejectReconfigure(selected,d) ==
    /\ s.phase = "active" /\ selected # {} /\ selected \subseteq {o \in Owners : s.claims[o] # None}
    /\ selected \cap s.updated = {} /\ Known /\ ~Consistent(NextClaims(selected,d))
    /\ s' = [s EXCEPT !.updated = @ \cup selected, !.rejectedUpdate = TRUE,
             !.lastSelected = selected, !.lastClaims = s.claims, !.lastRestore = FALSE, !.lastRejected = TRUE,
             !.rejectedSnapshot = <<s.current,s.claims,s.parent,s.original,s.originalParent>>]
UserOther ==
    /\ s.phase = "active" /\ ~s.otherEdited
    /\ s' = [s EXCEPT !.other = ~@, !.expectedOther = ~@, !.otherEdited = TRUE,
             !.parent = IF ~s.other THEN TRUE ELSE @, !.lastRestore = FALSE, !.lastRejected = FALSE]
UserOwned ==
    /\ s.phase = "active" /\ HasClaims(s.claims) /\ ~s.ownedEdited
    /\ s' = [s EXCEPT !.current = External, !.ownedEdited = TRUE, !.tampered = TRUE, !.lastRestore = FALSE, !.lastRejected = FALSE]
Block == /\ s.phase = "active" /\ s.captured /\ ~Known /\ s' = [s EXCEPT !.phase = "unknown"]
Finish == /\ s.phase = "active" /\ s.joined \cup s.denied = Owners /\ ~HasClaims(s.claims) /\ s' = [s EXCEPT !.phase = "done"]
Terminal == s.phase \in {"done","unknown"}
Done == /\ Terminal /\ UNCHANGED vars
Progress == (\E o \in Owners : Join(o) \/ Conflict(o) \/ Retire(o))
         \/ (\E selected \in SUBSET Owners, d \in {"on","off"} : Reconfigure(selected,d) \/ RejectReconfigure(selected,d))
         \/ Block \/ Finish
Next == Progress \/ UserOther \/ UserOwned \/ Done
Spec == Init /\ [][Next]_vars /\ WF_vars(Progress)
UnownedPreserved == s.other = s.expectedOther
OriginalNotResampled == s.captured => /\ s.original = s.witness /\ s.originalParent = s.parentWitness
ClaimsAgree == \A a,b \in Owners : s.claims[a] # None /\ s.claims[b] # None => s.claims[a] = s.claims[b]
ActiveValueKnown == (HasClaims(s.claims) /\ ~s.tampered) => s.current \in {s.original,s.applied}
ExternalOwnedPreserved == s.tampered => s.current = External
LastOwnerRestoresOriginal == (s.lastRestore /\ ~s.tampered) => s.current = s.original
OriginalEmptyParentPreserved == (s.lastRestore /\ s.originalParent # "missing") => s.parent
NewEmptyParentRemoved == (s.lastRestore /\ s.originalParent = "missing" /\ s.original = Missing /\ ~s.other) => ~s.parent
NoForeignOwnerRetirement == s.lastSelected # {} => \A o \in Owners \ s.lastSelected : s.claims[o] = s.lastClaims[o]
OtherOwnerKeepsApplied == (s.lastSelected # {} /\ HasClaims(s.claims) /\ ~s.tampered) => s.current = s.applied
NoWriteOnRejectedReplacement == s.lastRejected => <<s.current,s.claims,s.parent,s.original,s.originalParent>> = s.rejectedSnapshot
EventuallyTerminal == <>Terminal
NoSharedAtomicUpdate == ~(s.sharedUpdate /\ s.phase = "done")
NoChangedUpdate == ~(s.changedUpdate /\ s.phase = "done")
NoRejectedReplacement == ~(s.rejectedUpdate /\ s.phase = "done")
NoSharedOwnerRetirement == ~(s.sharedSeen /\ s.phase = "done" /\ s.retired = Owners)
NoConflictRetirement == ~(s.denied # {} /\ s.phase = "done")
NoMissingParentRestore == ~(s.phase = "done" /\ s.originalParent = "missing" /\ ~s.parent)
NoEmptyParentRestore == ~(s.phase = "done" /\ s.originalParent = "empty" /\ s.parent)
NoExternalOwnedBlock == ~(s.phase = "unknown" /\ s.tampered)
=============================================================================
