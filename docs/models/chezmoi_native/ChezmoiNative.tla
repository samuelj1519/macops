-------------------------- MODULE ChezmoiNative --------------------------
EXTENDS Naturals, TLC
VARIABLES phase, dry, authorized, stable, runs, queryRan, alive, nativeExit,
          verified, crashed, recovered, partial
vars == <<phase, dry, authorized, stable, runs, queryRan, alive, nativeExit,
          verified, crashed, recovered, partial>>
Init ==
 /\ phase = "planned"
 /\ dry \in BOOLEAN
 /\ authorized \in BOOLEAN
 /\ stable = TRUE
 /\ runs = 0
 /\ queryRan = FALSE
 /\ alive = FALSE
 /\ nativeExit = "none"
 /\ verified = FALSE
 /\ crashed = FALSE
 /\ recovered = FALSE
 /\ partial = FALSE
Preview ==
 /\ phase = "planned" /\ dry
 /\ phase' = "previewed"
 /\ UNCHANGED <<dry, authorized, stable, runs, queryRan, alive, nativeExit,
                verified, crashed, recovered, partial>>
BlockUnapproved ==
 /\ phase = "planned" /\ ~dry /\ ~authorized
 /\ phase' = "blocked"
 /\ UNCHANGED <<dry, authorized, stable, runs, queryRan, alive, nativeExit,
                verified, crashed, recovered, partial>>
Prepare ==
 /\ phase = "planned" /\ ~dry /\ authorized
 /\ queryRan' = TRUE /\ phase' = "prepared"
 /\ UNCHANGED <<dry, authorized, stable, runs, alive, nativeExit,
                verified, crashed, recovered, partial>>
Invalidate ==
 /\ phase \in {"prepared", "running", "exited"} /\ stable
 /\ stable' = FALSE
 /\ UNCHANGED <<phase, dry, authorized, runs, queryRan, alive, nativeExit,
                verified, crashed, recovered, partial>>
BlockStale ==
 /\ phase = "prepared" /\ ~stable
 /\ phase' = "blocked"
 /\ UNCHANGED <<dry, authorized, stable, runs, queryRan, alive, nativeExit,
                verified, crashed, recovered, partial>>
RunOnce ==
 /\ phase = "prepared" /\ stable /\ runs = 0 /\ authorized /\ ~dry
 /\ phase' = "running" /\ runs' = runs + 1 /\ alive' = TRUE
 /\ UNCHANGED <<dry, authorized, stable, queryRan, nativeExit,
                verified, crashed, recovered, partial>>
ExitNative ==
 /\ phase = "running"
 /\ nativeExit' \in {"ok", "failed"}
 /\ partial' \in BOOLEAN
 /\ phase' = "exited" /\ alive' = FALSE
 /\ UNCHANGED <<dry, authorized, stable, runs, queryRan, verified,
                crashed, recovered>>
Timeout ==
 /\ phase = "running"
 /\ phase' = "unknown" /\ alive' = FALSE /\ nativeExit' = "unknown"
 /\ partial' \in BOOLEAN
 /\ UNCHANGED <<dry, authorized, stable, runs, queryRan, verified,
                crashed, recovered>>
Verify ==
 /\ phase = "exited" /\ stable /\ authorized
 /\ verified' \in BOOLEAN /\ queryRan' = TRUE
 /\ phase' = IF nativeExit = "ok" /\ verified' THEN "complete"
              ELSE IF nativeExit = "failed" THEN "partial" ELSE "unknown"
 /\ UNCHANGED <<dry, authorized, stable, runs, alive, nativeExit,
                crashed, recovered, partial>>
CannotVerify ==
 /\ phase = "exited" /\ ~stable
 /\ phase' = "unknown"
 /\ UNCHANGED <<dry, authorized, stable, runs, queryRan, alive, nativeExit,
                verified, crashed, recovered, partial>>
Crash ==
 /\ phase \in {"prepared", "running", "exited"} /\ ~crashed
 /\ phase' = IF runs = 0 THEN "failed" ELSE "unknown"
 /\ crashed' = TRUE
 /\ nativeExit' = IF phase = "running" THEN "unknown" ELSE nativeExit
 /\ UNCHANGED <<dry, authorized, stable, runs, queryRan, alive,
                verified, recovered, partial>>
ObserveEnded ==
 /\ phase = "unknown" /\ alive
 /\ alive' = FALSE
 /\ UNCHANGED <<phase, dry, authorized, stable, runs, queryRan, nativeExit,
                verified, crashed, recovered, partial>>
RecoverQuery ==
 /\ phase = "unknown" /\ ~alive /\ stable /\ authorized /\ ~recovered
 /\ verified' \in BOOLEAN /\ queryRan' = TRUE /\ recovered' = TRUE
 /\ phase' = IF nativeExit = "ok" /\ verified' THEN "complete" ELSE "unknown"
 /\ UNCHANGED <<dry, authorized, stable, runs, alive, nativeExit,
                crashed, partial>>
ResolveFailed ==
 /\ phase = "unknown" /\ ~alive
 /\ phase' = "failed"
 /\ UNCHANGED <<dry, authorized, stable, runs, queryRan, alive, nativeExit,
                verified, crashed, recovered, partial>>
Terminal == phase \in {"previewed", "blocked", "complete", "failed", "partial"}
            \/ (phase = "unknown" /\ ~alive)
Done == Terminal /\ UNCHANGED vars
Next == Preview \/ BlockUnapproved \/ Prepare \/ Invalidate \/ BlockStale
        \/ RunOnce \/ ExitNative \/ Timeout \/ Verify \/ CannotVerify \/ Crash
        \/ ObserveEnded \/ RecoverQuery \/ ResolveFailed \/ Done
Spec == Init /\ [][Next]_vars /\ WF_vars(Next) /\ WF_vars(ObserveEnded)
TypeOK ==
 /\ phase \in {"planned", "previewed", "blocked", "prepared", "running",
               "exited", "unknown", "complete", "failed", "partial"}
 /\ nativeExit \in {"none", "ok", "failed", "unknown"}
 /\ runs \in Nat
 /\ <<dry, authorized, stable, queryRan, alive, verified, crashed, recovered, partial>>
      \in BOOLEAN \X BOOLEAN \X BOOLEAN \X BOOLEAN \X BOOLEAN \X BOOLEAN
                  \X BOOLEAN \X BOOLEAN \X BOOLEAN
DryNoNative == dry => (runs = 0 /\ ~queryRan)
NoUnapprovedNative == ~authorized => (runs = 0 /\ ~queryRan)
AtMostOnce == runs <= 1
NoInventedSuccess == phase = "complete" =>
 (nativeExit = "ok" /\ verified /\ stable /\ ~alive /\ runs = 1 /\ authorized /\ ~dry)
NoReplayOnRecovery == recovered => runs = 1
NoRepeatRunAction == [][~(runs >= 1 /\ RunOnce)]_vars
RunsNeverDecrease == [][runs' >= runs]_vars
NoRollbackClaim == phase # "rolled_back"
EventualStable == <>Terminal
NoPreview == phase # "previewed"
NoBlockedAuthorization == ~(phase = "blocked" /\ ~authorized /\ ~dry)
NoSuccess == phase # "complete"
NoNativePartial == ~(phase = "partial" /\ partial)
NoMismatch == ~(phase = "unknown" /\ nativeExit = "ok" /\ ~verified /\ stable)
NoUnknownFilesMatch == ~(phase = "unknown" /\ nativeExit = "unknown" /\ verified /\ recovered)
NoCrashRecovery == ~(crashed /\ recovered)
NoStaleBlock == ~(phase = "blocked" /\ ~stable)
=============================================================================
