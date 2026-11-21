----------------------- MODULE BrewRecoveryHistory -----------------------
VARIABLES s
vars == <<s>>

Terminal == {"succeeded", "failed"}
InitialRows ==
    {[active |-> a, history |-> h, sameOperation |-> same, originalReceipt |-> receipt,
      pathsUnchanged |-> unchanged, dry |-> preview, proof |-> <<a, h, receipt>>,
      result |-> "undecided"] : a \in {"pending", "other", "missing"} \cup Terminal,
      h \in {"pending", "missing"} \cup Terminal, same \in BOOLEAN,
      receipt \in BOOLEAN, unchanged \in BOOLEAN, preview \in BOOLEAN}
Init == s \in {row \in InitialRows : row.history = "succeeded" => row.originalReceipt}

Select == CASE s.dry -> "preview"
            [] s.active \in Terminal -> "terminal"
            [] s.active # "pending" -> IF s.history \in Terminal THEN "history-only" ELSE "blocked"
            [] ~s.sameOperation -> "blocked"
            [] s.history \in Terminal -> IF s.pathsUnchanged THEN "aligned" ELSE "blocked"
            [] OTHER -> "needs-original-evidence"

Recover ==
    /\ s.result = "undecided"
    /\ s' = [s EXCEPT !.result = Select, !.active = IF Select = "aligned" THEN s.history ELSE @]

Done == /\ s.result # "undecided" /\ UNCHANGED vars
Next == Recover \/ Done
Spec == Init /\ [][Next]_vars /\ WF_vars(Recover)

NeverChangesHistoryOrReceipt == <<s.history, s.originalReceipt>> = <<s.proof[2], s.proof[3]>>
ChangesOnlyMatchingPending == s.active # s.proof[1] =>
    /\ s.proof[1] = "pending"
    /\ s.sameOperation /\ s.pathsUnchanged /\ ~s.dry
    /\ s.result = "aligned" /\ s.active = s.history /\ s.history \in Terminal
PreviewZeroWrites == s.dry => s.active = s.proof[1]
NoEvidenceInferred == s.result = "aligned" => s.proof[2] \in Terminal
EventuallyDecided == <>(s.result # "undecided")

NoSucceededGapAligned == ~(s.result = "aligned" /\ s.active = "succeeded" /\ s.originalReceipt)
NoFailedGapAligned == ~(s.result = "aligned" /\ s.active = "failed")
NoForeignActiveBlocked == ~(s.result = "blocked" /\ s.active = "other" /\ s.history = "pending")
NoReceiptWithoutTerminalNeedsProof == ~(s.result = "needs-original-evidence" /\ s.originalReceipt)
NoMovedRecordBlocked == ~(s.result = "blocked" /\ s.active = "pending" /\ s.history \in Terminal /\ s.sameOperation /\ ~s.pathsUnchanged)
=============================================================================
