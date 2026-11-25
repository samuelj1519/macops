--------------------------- MODULE MainJourneys ---------------------------
CONSTANT Legacy
VARIABLE s
Ends == {"preview", "cancelled", "blocked", "complete", "history", "config", "native-result"}
Init ==
    \E story \in {"preview", "batch", "history", "config", "native-result"}, pending \in BOOLEAN:
        s = [story |-> story, pending |-> pending, phase |-> "start",
             wrote |-> FALSE, showPlan |-> FALSE, reportedSkipped |-> FALSE,
             originalFound |-> FALSE, configIncomplete |-> FALSE, saysClean |-> FALSE]
OpenPreview ==
    s.phase = "start" /\ s.story = "preview"
    /\ s' = [s EXCEPT !.phase = "preview", !.showPlan = ~Legacy]
OpenBatch == s.phase = "start" /\ s.story = "batch" /\ s' = [s EXCEPT !.phase = "planned"]
FindHistory ==
    s.phase = "start" /\ s.story = "history"
    /\ s' = [s EXCEPT !.phase = "history", !.originalFound = ~Legacy]
NativeResult == s.phase = "start" /\ s.story = "native-result"
    /\ s' = [s EXCEPT !.phase = "native-result", !.originalFound = ~Legacy]
CheckConfig ==
    s.phase = "start" /\ s.story = "config"
    /\ \E partial \in BOOLEAN:
        s' = [s EXCEPT !.phase = "config", !.configIncomplete = partial, !.saysClean = Legacy \/ ~partial]
Cancel == s.phase = "planned" /\ s' = [s EXCEPT !.phase = "cancelled"]
NewPending == s.phase = "planned" /\ ~s.pending /\ s' = [s EXCEPT !.pending = TRUE]
Block == s.phase = "planned" /\ s.pending /\ s' = [s EXCEPT !.phase = "blocked"]
Dispatch == s.phase = "planned" /\ ~s.pending
            /\ s' = [s EXCEPT !.phase = "running", !.wrote = TRUE]
Finish == s.phase = "running" /\ s' = [s EXCEPT !.phase = "complete"]
Unknown == s.phase = "running"
           /\ s' = [s EXCEPT !.phase = "stopped", !.reportedSkipped = ~Legacy]
Recover == s.phase = "stopped" /\ s' = [s EXCEPT !.phase = "history", !.originalFound = ~Legacy]
Done == s.phase \in Ends /\ UNCHANGED s
Next == OpenPreview \/ OpenBatch \/ FindHistory \/ NativeResult \/ CheckConfig \/ Cancel \/ NewPending
        \/ Block \/ Dispatch \/ Finish \/ Unknown \/ Recover \/ Done

ClearPreview == s.phase = "preview" => s.showPlan /\ ~s.wrote
NoWriteWhileBlocked == s.wrote => s.story = "batch" /\ ~s.pending
StoppedItemsAreVisible == s.phase = "stopped" => s.reportedSkipped
OriginalNumberFindsRecord == s.phase = "history" => s.originalFound
NativeResultShowsOriginal == s.phase = "native-result" => s.originalFound
PartialDoesNotMeanClean == s.configIncomplete => ~s.saysClean
=========================================================================
