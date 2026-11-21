---------------------------- MODULE NativeSkills ----------------------------
EXTENDS Naturals, TLC
CONSTANTS Mode, Action, References, RegistryValid, Delivery, Steps, Already
VARIABLES phase, nativeCalls, process, nativeVerified, payload, metadata,
          patched, receipt, foreignEdited, crashUsed, touchedNativeLock,
          touchedOtherClient, overwroteForeign, queried, otherNativeChange, artifact, binding, receiptIds, receiptBinding
vars == <<phase,nativeCalls,process,nativeVerified,payload,metadata,patched,
          receipt,foreignEdited,crashUsed,touchedNativeLock,touchedOtherClient,
          overwroteForeign,queried,otherNativeChange,artifact,binding,receiptIds,receiptBinding>>
Terminal == {"Verified", "Blocked", "Preview"}
Init == /\ phase="Planned" /\ nativeCalls=0 /\ process="Idle"
        /\ nativeVerified=FALSE /\ payload="Before"
        /\ metadata=[n \in 1..Steps |-> IF n \in Already THEN "After" ELSE "Before"] /\ patched={}
        /\ receipt=FALSE /\ foreignEdited=FALSE /\ crashUsed=FALSE
        /\ touchedNativeLock=FALSE /\ touchedOtherClient=FALSE
        /\ overwroteForeign=FALSE /\ queried=FALSE /\ otherNativeChange=FALSE
        /\ artifact=(IF Delivery="derived" THEN "Missing" ELSE "Ready")
        /\ binding="old-source" /\ receiptIds={"slot"} /\ receiptBinding="old-source"
Preview == /\ phase="Planned" /\ Mode="dry"
           /\ phase'="Preview" /\ UNCHANGED <<nativeCalls,process,nativeVerified,
              payload,metadata,patched,receipt,foreignEdited,crashUsed,
              touchedNativeLock,touchedOtherClient,overwroteForeign,queried,otherNativeChange,artifact,binding,receiptIds,receiptBinding>>
Start == /\ phase \in {"Planned","Unknown"} /\ Mode="write"
         /\ nativeCalls=0
         /\ RegistryValid /\ (Action="install" \/ References="none")
         /\ (Action="remove" \/ Delivery="direct" \/ artifact="Ready")
         /\ phase'="Running" /\ nativeCalls'=nativeCalls+1 /\ process'="Running"
         /\ UNCHANGED <<nativeVerified,payload,metadata,patched,receipt,
              foreignEdited,crashUsed,touchedNativeLock,touchedOtherClient,
              overwroteForeign,queried,otherNativeChange,artifact,binding,receiptIds,receiptBinding>>
NativeEffect == /\ process="Running" /\ payload="Before"
                /\ phase \in {"Running","Unknown"}
                /\ payload' \in {"Native","Partial"}
                /\ otherNativeChange' \in BOOLEAN
                /\ binding'=(IF Delivery="derived" THEN "artifact-v2" ELSE "github-commit2")
                /\ UNCHANGED <<phase,nativeCalls,process,nativeVerified,metadata,
                   patched,receipt,foreignEdited,crashUsed,touchedNativeLock,
                   touchedOtherClient,overwroteForeign,queried,artifact,receiptIds,receiptBinding>>
Complete == /\ process="Running" /\ payload \in {"Native","Partial"}
            /\ phase \in {"Running","Unknown"}
            /\ process' \in {"Success","Partial"}
            /\ UNCHANGED <<phase,nativeCalls,nativeVerified,payload,metadata,patched,
               receipt,foreignEdited,crashUsed,touchedNativeLock,
               touchedOtherClient,overwroteForeign,queried,otherNativeChange,artifact,binding,receiptIds,receiptBinding>>
Query == /\ phase \in {"Running","Unknown"}
         /\ process \in {"Success","Partial"}
         /\ queried'=TRUE
         /\ phase'=(IF process="Success" /\ payload="Native" /\ ~otherNativeChange
                    /\ (Action="remove" \/ Delivery="direct" \/ artifact="Ready")
                    THEN "NativeSucceeded" ELSE "Blocked")
         /\ nativeVerified'=(phase'="NativeSucceeded")
         /\ UNCHANGED <<nativeCalls,process,payload,metadata,patched,receipt,
              foreignEdited,crashUsed,touchedNativeLock,touchedOtherClient,
              overwroteForeign,otherNativeChange,artifact,binding,receiptIds,receiptBinding>>
BeginCustomization == /\ phase="NativeSucceeded" /\ nativeVerified /\ Action="install"
                      /\ phase'="CustomizationPending"
                      /\ UNCHANGED <<nativeCalls,process,nativeVerified,
                         payload,metadata,patched,receipt,foreignEdited,
                         crashUsed,touchedNativeLock,touchedOtherClient,
                         overwroteForeign,queried,otherNativeChange,artifact,binding,receiptIds,receiptBinding>>
Patch(n) == /\ phase="CustomizationPending" /\ n \notin patched
            /\ payload="Native" /\ metadata[n]="Before"
            /\ metadata'=[metadata EXCEPT ![n]="After"]
            /\ patched'=patched \cup {n}
            /\ overwroteForeign'=(overwroteForeign \/ metadata[n]="Foreign")
            /\ UNCHANGED <<phase,nativeCalls,process,nativeVerified,payload,
               receipt,foreignEdited,crashUsed,touchedNativeLock,
               touchedOtherClient,queried,otherNativeChange,artifact,binding,receiptIds,receiptBinding>>
DetectDrift == /\ phase \in {"NativeSucceeded","CustomizationPending","Recovering"}
               /\ (payload#"Native" \/ \E n \in 1..Steps: metadata[n]="Foreign")
               /\ phase'="Blocked"
               /\ UNCHANGED <<nativeCalls,process,nativeVerified,payload,
                  metadata,patched,receipt,foreignEdited,crashUsed,
                  touchedNativeLock,touchedOtherClient,overwroteForeign,queried,otherNativeChange,artifact,binding,receiptIds,receiptBinding>>
Finish == /\ phase="CustomizationPending" /\ patched=1..Steps
          /\ payload="Native" /\ \A n \in 1..Steps: metadata[n]="After"
          /\ phase'="Verified" /\ receipt'=TRUE
          /\ receiptIds'=receiptIds \cup {"slot"} /\ receiptBinding'=binding
          /\ UNCHANGED <<nativeCalls,process,nativeVerified,payload,metadata,
             patched,foreignEdited,crashUsed,touchedNativeLock,
             touchedOtherClient,overwroteForeign,queried,otherNativeChange,artifact,binding>>
Crash == /\ ~crashUsed
         /\ phase \in {"Running","NativeSucceeded","CustomizationPending"}
         /\ (Action="install" \/ phase="Running")
         /\ phase'=(IF nativeVerified THEN "Recovering" ELSE "Unknown")
         /\ crashUsed'=TRUE
         /\ UNCHANGED <<nativeCalls,process,nativeVerified,payload,metadata,
              patched,receipt,foreignEdited,touchedNativeLock,
              touchedOtherClient,overwroteForeign,queried,otherNativeChange,artifact,binding,receiptIds,receiptBinding>>
Recover == /\ phase="Recovering" /\ nativeVerified /\ payload="Native"
           /\ \A n \in 1..Steps: metadata[n]#"Foreign"
           /\ phase'="CustomizationPending"
           /\ UNCHANGED <<nativeCalls,process,nativeVerified,payload,metadata,
                patched,receipt,foreignEdited,crashUsed,touchedNativeLock,
                touchedOtherClient,overwroteForeign,queried,otherNativeChange,artifact,binding,receiptIds,receiptBinding>>
External == /\ ~foreignEdited
            /\ phase \in {"NativeSucceeded","CustomizationPending","Recovering"}
            /\ foreignEdited'=TRUE
            /\ \/ /\ payload'="Foreign" /\ UNCHANGED metadata
               \/ /\ payload'=payload
                   /\ \E n \in 1..Steps: metadata'=[metadata EXCEPT ![n]="Foreign"]
            /\ UNCHANGED <<phase,nativeCalls,process,nativeVerified,patched,
                 receipt,crashUsed,touchedNativeLock,touchedOtherClient,
                 overwroteForeign,queried,otherNativeChange,artifact,binding,receiptIds,receiptBinding>>
PrepareArtifact == /\ phase="Planned" /\ Mode="write" /\ Action="install" /\ Delivery="derived"
                   /\ artifact="Missing" /\ artifact'="Ready"
                   /\ UNCHANGED <<phase,nativeCalls,process,nativeVerified,payload,
                      metadata,patched,receipt,foreignEdited,crashUsed,touchedNativeLock,
                      touchedOtherClient,overwroteForeign,queried,otherNativeChange,
                      binding,receiptIds,receiptBinding>>
ArtifactFailure == /\ phase="Planned" /\ Mode="write" /\ Action="install" /\ Delivery="derived"
                   /\ artifact="Missing" /\ phase'="Blocked"
                   /\ UNCHANGED <<nativeCalls,process,nativeVerified,payload,
                      metadata,patched,receipt,foreignEdited,crashUsed,touchedNativeLock,
                      touchedOtherClient,overwroteForeign,queried,otherNativeChange,
                      artifact,binding,receiptIds,receiptBinding>>
AcceptExisting(n) == /\ phase="CustomizationPending" /\ n \notin patched
                     /\ metadata[n]="After" /\ payload="Native"
                     /\ patched'=patched \cup {n}
                     /\ UNCHANGED <<phase,nativeCalls,process,nativeVerified,payload,
                          metadata,receipt,foreignEdited,crashUsed,touchedNativeLock,
                          touchedOtherClient,overwroteForeign,queried,otherNativeChange,
                          artifact,binding,receiptIds,receiptBinding>>
Reject == /\ phase="Planned" /\ Mode="write"
          /\ (~RegistryValid \/ (Action="remove" /\ References#"none"))
          /\ phase'="Blocked"
          /\ UNCHANGED <<nativeCalls,process,nativeVerified,payload,metadata,
               patched,receipt,foreignEdited,crashUsed,touchedNativeLock,
               touchedOtherClient,overwroteForeign,queried,otherNativeChange,artifact,binding,receiptIds,receiptBinding>>
FinishRemoval == /\ phase="NativeSucceeded" /\ Action="remove"
                 /\ nativeVerified /\ payload="Native" /\ ~otherNativeChange
                 /\ phase'="Verified" /\ receipt'=TRUE
          /\ receiptIds'=receiptIds \cup {"slot"} /\ receiptBinding'=binding
                 /\ UNCHANGED <<nativeCalls,process,nativeVerified,payload,
                      metadata,patched,foreignEdited,crashUsed,touchedNativeLock,
                      touchedOtherClient,overwroteForeign,queried,otherNativeChange,artifact,binding>>
Done == /\ phase \in Terminal /\ UNCHANGED vars
Progress == Preview \/ PrepareArtifact \/ ArtifactFailure \/ Reject \/ FinishRemoval \/ Start \/ NativeEffect \/ Complete \/ Query \/ BeginCustomization
            \/ (\E n \in 1..Steps: Patch(n) \/ AcceptExisting(n)) \/ DetectDrift \/ Finish \/ Recover \/ Done
Next == Progress \/ Crash \/ External
Spec == Init /\ [][Next]_vars /\ WF_vars(Progress)
TypeOK == /\ phase \in {"Planned","Running","Unknown","NativeSucceeded",
                     "CustomizationPending","Recovering","Verified","Blocked","Preview"}
          /\ nativeCalls \in Nat /\ patched \subseteq 1..Steps
          /\ process \in {"Idle","Running","Success","Partial"}
          /\ payload \in {"Before","Native","Partial","Foreign"}
          /\ metadata \in [1..Steps -> {"Before","After","Foreign"}]
          /\ artifact \in {"Missing","Ready"}
          /\ binding \in {"old-source","artifact-v2","github-commit2"}
          /\ receiptBinding \in {"old-source","artifact-v2","github-commit2"}
          /\ receiptIds \subseteq {"slot","artifact-v2","github-commit2"}
          /\ \A flag \in {nativeVerified,receipt,foreignEdited,crashUsed,
               touchedNativeLock,touchedOtherClient,overwroteForeign,queried,otherNativeChange}: flag \in BOOLEAN
OneNativeCall == nativeCalls<=1
NoRepeat == [][nativeCalls=0 \/ nativeCalls'=nativeCalls]_vars
DryRunNoWrites == Mode="dry" => nativeCalls=0 /\ patched={} /\ ~receipt
DryRunNoArtifact == Mode="dry" /\ Delivery="derived" => artifact="Missing"
NativeAndOtherOwnersUntouched == ~touchedNativeLock /\ ~touchedOtherClient
NoForeignOverwrite == ~overwroteForeign
CustomizationAfterNative == patched#{} => nativeVerified /\ queried /\ process="Success"
ReceiptRequiresAllStages == receipt => phase="Verified" /\ nativeVerified
                           /\ queried /\ payload="Native" /\ ~otherNativeChange
                           /\ (Action="remove" \/ (patched=1..Steps /\ \A n \in 1..Steps: metadata[n]="After"))
PartialNeverSucceeds == process="Partial" => ~nativeVerified /\ ~receipt
RecoverNeverSpawns == crashUsed => nativeCalls=1
SharedReferenceBlocksRemoval == Action="remove" /\ References#"none" => nativeCalls=0
RegistryShapeBlocksNative == ~RegistryValid => nativeCalls=0
OtherClientChangesNotCertified == otherNativeChange => ~nativeVerified /\ ~receipt
DerivedNeedsReadyArtifact == Action="install" /\ Delivery="derived" /\ nativeCalls>0 => artifact="Ready"
StableInstallOwner == receiptIds \subseteq {"slot"}
ReceiptUsesActualSourceBinding == receipt => receiptBinding=binding
EventuallyTerminal == <> (phase \in Terminal)
NoArtifactFailure == ~(phase="Blocked" /\ Delivery="derived" /\ artifact="Missing" /\ nativeCalls=0)
NoLicenseConflictBlocked == ~(phase="Blocked" /\ Steps=4 /\ metadata[3]="Foreign")
NoArtifactReady == artifact#"Ready" \/ Delivery#"derived"
NoDerivedUpgradeSameSlot == ~(receipt /\ Delivery="derived" /\ binding="artifact-v2" /\ receiptIds={"slot"})
NoSameContentNoop == ~(receipt /\ Already#{})
NoSuccess == ~receipt
NoPartial == process#"Partial"
NoUnknownWithLivePayload == ~(phase="Unknown" /\ process="Running" /\ payload="Native")
NoUnknownAfterCrash == ~(phase="Unknown" /\ crashUsed)
NoRecoveryAfterFirstPatch == ~(phase="Recovering" /\ patched={1})
NoForeignBlocked == ~(phase="Blocked" /\ foreignEdited)
=============================================================================
