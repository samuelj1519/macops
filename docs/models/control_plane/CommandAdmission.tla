----------------------------- MODULE CommandAdmission -----------------------------
CONSTANTS LegacyEntry, LegacyHistory
VARIABLE s

Commands == {"worker", "settings", "migration", "ordinary", "history"}
Formats == {"json", "plain", "terminal"}
Records == {"none", "running", "codex", "claude", "finished", "superseded", "receipt", "unverified"}
Ends == {"worker", "settings", "migration", "blocked", "read", "write", "stale", "done"}
Browses == s.format # "json"
Followup == s.record \in {"running", "codex", "claude"}
Init ==
    \E command \in Commands, format \in Formats, pending \in BOOLEAN:
        /\ (command # "history" \/ format = "terminal")
        /\ s = [phase |-> "start", command |-> command, format |-> format,
                 pending |-> pending, blockedAtEntry |-> pending, record |-> "none",
                 offered |-> FALSE, writerGuard |-> FALSE, writer |-> "none"]

RunWorker == s.phase = "start" /\ s.command = "worker" /\ s' = [s EXCEPT !.phase = "worker"]
Setup == s.phase = "start" /\ s.command = "settings" /\ s' = [s EXCEPT !.phase = "settings"]
Migrate == s.phase = "start" /\ s.command = "migration" /\ s' = [s EXCEPT !.phase = "migration"]
Admit == s.phase = "start" /\ s.command \in {"ordinary", "history"}
         /\ (~s.pending \/ (LegacyEntry /\ Browses))
         /\ s' = [s EXCEPT !.phase = "admitted"]
Block == s.phase = "start" /\ s.command \in {"ordinary", "history"}
         /\ s.pending /\ ~(LegacyEntry /\ Browses)
         /\ s' = [s EXCEPT !.phase = "blocked"]
Read == s.phase = "admitted" /\ s.command = "ordinary" /\ s' = [s EXCEPT !.phase = "read"]
PreviewWrite ==
    /\ s.phase = "admitted" /\ s.command = "ordinary" /\ s.format = "terminal"
    /\ s' = [s EXCEPT !.phase = "preview", !.writer = "maintenance"]
SelectHistory ==
    /\ s.phase = "admitted" /\ s.command = "history"
    /\ \E record \in Records \ {"none"}:
         s' = [s EXCEPT !.phase = "selected", !.record = record,
               !.offered = record \in {"running", "codex"} \/ (record = "claude" /\ ~LegacyHistory)]
Leave == s.phase = "selected" /\ s' = [s EXCEPT !.phase = "done"]
Preview == s.phase = "selected" /\ s.offered /\ s' = [s EXCEPT !.phase = "preview", !.writer = "recovery"]
Cancel == s.phase = "preview" /\ s' = [s EXCEPT !.phase = "done"]
MarkerAppears ==
    s.phase \in {"admitted", "selected", "preview"} /\ ~s.pending
    /\ s' = [s EXCEPT !.pending = TRUE]
RecordChanges ==
    s.phase = "preview" /\ Followup
    /\ \E record \in {"finished", "superseded", "receipt", "unverified"}: s' = [s EXCEPT !.record = record]
EligibleWrite == ~s.pending /\ (s.writer = "maintenance" \/ Followup)
Confirm ==
    /\ s.phase = "preview" /\ EligibleWrite
    /\ s' = [s EXCEPT !.phase = "write", !.writerGuard = TRUE]
Reject ==
    /\ s.phase = "preview" /\ ~EligibleWrite
    /\ s' = [s EXCEPT !.phase = "stale"]
Done == s.phase \in Ends /\ UNCHANGED s

Next == RunWorker \/ Setup \/ Migrate \/ Admit \/ Block \/ Read \/ PreviewWrite \/ SelectHistory
        \/ Leave \/ Preview \/ Cancel \/ MarkerAppears \/ RecordChanges \/ Confirm \/ Reject \/ Done

SameAdmission ==
    s.command \in {"ordinary", "history"} /\ s.blockedAtEntry => s.phase \in {"start", "blocked"}
IndependentCommands ==
    s.command \in {"worker", "settings", "migration"} => s.phase \in {"start", s.command}
SameRecoveryChoices == s.phase = "selected" => s.offered = Followup
FinalGuard == s.phase = "write" => s.writerGuard /\ EligibleWrite
NoWriteWithoutConfirmation == s.writerGuard => s.phase = "write"
===================================================================================
