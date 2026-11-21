---------------------------- MODULE ModelAssist ----------------------------
EXTENDS Naturals

VARIABLE s
vars == <<s>>
Bits == {FALSE, TRUE}
ValidInput == s.requested /\ ~s.dry /\ s.publicBundle /\ s.endpointBound /\ s.profileValid
Init ==
    \E requested, dry, publicBundle, endpointBound, profileValid, hasKey \in Bits:
    s = [requested |-> requested, dry |-> dry, publicBundle |-> publicBundle,
         endpointBound |-> endpointBound, profileValid |-> profileValid, hasKey |-> hasKey,
         phase |-> "rules", expired |-> FALSE, reads |-> 0, sends |-> 0, confirmed |-> FALSE,
         advice |-> "rules", mayWrite |-> FALSE]
Credential ==
    /\ s.phase = "rules" /\ ValidInput
    /\ s' = [s EXCEPT !.reads = @ + 1, !.phase = IF s.hasKey THEN "key" ELSE "fallback"]
Skip ==
    /\ s.phase = "rules" /\ ~ValidInput
    /\ s' = [s EXCEPT !.phase = "fallback"]
Send ==
    /\ s.phase = "key" /\ ValidInput /\ s.hasKey /\ ~s.expired
    /\ s' = [s EXCEPT !.sends = @ + 1, !.phase = "waiting"]
Accept ==
    /\ s.phase = "waiting"
    /\ \E verified \in Bits:
      s' = [s EXCEPT !.confirmed = verified,
                    !.advice = IF verified THEN "annotated" ELSE "rules",
                    !.phase = "done"]
Timeout ==
    /\ s.phase \in {"rules", "key", "waiting"}
    /\ s' = [s EXCEPT !.phase = "fallback", !.expired = TRUE, !.advice = "rules"]
Next == Credential \/ Skip \/ Send \/ Accept \/ Timeout
CredentialOnlyForExplicitPublicCall == s.reads > 0 => ValidInput
SendOnlyOnceWithBoundIdentity == s.sends \in 0..1 /\ (s.sends > 0 => ValidInput /\ s.hasKey)
OnlyBoundAnnotations == s.advice = "annotated" => s.confirmed /\ s.sends = 1
TimedOutUsesRules == s.expired => s.phase = "fallback" /\ s.advice = "rules"
NeverGrantsWrites == ~s.mayWrite
AbsentAnnotation == s.advice # "annotated"
AbsentMissingKey == ~(s.phase = "fallback" /\ s.reads = 1 /\ ~s.hasKey)
AbsentTimeout == ~(s.expired /\ s.sends = 1)
AbsentBadOutput == ~(s.phase = "done" /\ s.sends = 1 /\ s.advice = "rules")
=============================================================================
