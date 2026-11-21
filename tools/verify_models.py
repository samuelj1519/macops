#!/usr/bin/env python3
"""Verify all TLA+ models and non-vacuity reachability witnesses via TLC."""
import os
import argparse
import json
from pathlib import Path
import re
import subprocess
import tempfile

TLA_JAR = Path(os.environ.get("TLA2TOOLS_JAR", "tla2tools.jar")).resolve()

MODELS = [
    {
        "name": "uv_observed_r2",
        "tla": "docs/models/uv_observed/UvObserved.tla",
        "cfg": "docs/models/uv_observed/UvObserved.cfg",
        "witness_cfg_from_baseline": True,
        "witnesses": [
            "NoAdoptSuccess", "NoCrashRecovery", "NoAliceReuse", "NoBobReuse",
            "NoCASUnknown", "NoLateOwnerUnknown", "NoLateSingletonUnknown",
            "NoDoubleOwnerBlocked", "NoReadSuccess",
        ],
    },
    {
        "name": "config_tx",
        "tla": "docs/models/config_tx/ConfigTx.tla",
        "cfg": "docs/models/config_tx/ConfigTx.cfg",
        "witnesses": [
            "NoApplySuccess",
            "NoRollbackRecovery",
            "NoExternalConflictUnknown",
            "NoCrashAfterCommitRecovery",
        ],
    },
    {
        "name": "capability_tx",
        "tla": "docs/models/capability_tx/CapabilityTx.tla",
        "cfg": "docs/models/capability_tx/CapabilityTx.cfg",
        "witnesses": [
            "NoSyncSuccess",
            "NoPolicyRecoverNoop",
            "NoInterruptedPendingRecovery",
            "NoUpgradeOverlayRebaseSuccess",
            "NoTamperUnknown",
            "NoNativePartialRecovery",
            "NoOverlayPartialRecovery",
            "NoLedgerTamperBlocked",
            "NoRollbackCrashRecovery",
            "NoSyncedNoop",
        ],
    },
    {
        "name": "source_upgrade",
        "tla": "docs/models/source_upgrade/SourceUpgrade.tla",
        "cfg": "docs/models/source_upgrade/SourceUpgrade.cfg",
        "witnesses": ['NoUpgradeSuccess', 'NoAdoptionSuccess', 'NoNativePartialRecovery', 'NoOwnRestoredBefore', 'NoCrashAfterClearSuccess', 'NoForeignRootUnknown', 'NoUnknownInodeBlocked', 'NoUnchangedLedgerSuccess', 'NoLateCASForeignRetained', 'NoAdoptPartialRollback', 'NoRepeatedOwnRestoreRecovery', 'NoUpgradeAfterClearRecovery', 'NoDeclaredWithoutReceiptSuccess'],
    },
    {
        "name": "source_ledger_on",
        "tla": "docs/models/source_upgrade/SourceLedgerON.tla",
        "cfg": "docs/models/source_upgrade/SourceLedgerON.cfg",
        "witnesses": [
            "NoForwardExistingTarget", "NoForwardExistingStage", "NoForwardMissingTarget", "NoForwardMissingStage",
            "NoReverseExistingTarget", "NoReverseExistingStage", "NoReverseMissingTarget", "NoReverseMissingStage",
            "NoActualRollbackClearGap", "NoTwoCrashesAfterReverse", "NoCompleteAfterClearGap",
            "NoRetainedDeleteBeforeClear", "NoRetainedDeleteAfterClear", "NoEvidenceLossBlocked",
            "NoPathCollisionRejected", "NoIdentityAliasRejected", "NoCrossVolumeRejected",
            "NoBudgetRejected", "NoParentRejected", "NoRootOverlapRejected",
        ],
    },
    {
        "name": "profile_config",
        "tla": "docs/models/profile_config/ProfileConfig.tla",
        "cfg": "docs/models/profile_config/ProfileConfig.cfg",
        "constants": 'CONSTANTS\n Missing = "missing"\n External = "external"\n',
        "witnesses": [
            "NoSharedOwnerRetirement", "NoConflictRetirement", "NoMissingParentRestore",
            "NoEmptyParentRestore", "NoExternalOwnedBlock", "NoChangedUpdate",
            "NoRejectedReplacement", "NoSharedAtomicUpdate",
        ],
    },
    {
        "name": "ai_upgrade",
        "tla": "docs/models/ai_upgrade/AiUpgrade.tla",
        "cfg": "docs/models/ai_upgrade/AiUpgrade.cfg",
        "witnesses": [
            "NoReconciledUpgradeSuccess",
            "NoGateRevertDriftedForceLive",
            "NoSourceWriteFailUnknown",
            "NoDriftBlocked",
        ],
    },
    {
        "name": "migration",
        "tla": "docs/models/migration/Migration.tla",
        "cfg": "docs/models/migration/Migration.cfg",
        "witnesses": [
            "NoMigrationSuccess",
            "NoPartialPlacementCrashRollback",
            "NoPostSwitchPreserved",
            "NoDivergenceBlocked",
            "NoLateDivergenceAfterCleanVerify",
        ],
    },
    {
        "name": "migration_legacy_writers",
        "tla": "docs/models/migration/LegacyWriters.tla",
        "cfg": "docs/models/migration/LegacyWriters.cfg",
        "witnesses": [
            "NoRestoredSourceBeforeRun",
            "NoRestoredEntrypointBeforePlace",
            "NoIdentityDriftBeforeCommit",
            "NoPendingCleanup",
            "NoEnableNew",
        ],
    },
    {
        "name": "brew_recovery_history",
        "tla": "docs/models/brew_native/BrewRecoveryHistory.tla",
        "cfg": "docs/models/brew_native/BrewRecoveryHistory.cfg",
        "witnesses": [
            "NoSucceededGapAligned",
            "NoFailedGapAligned",
            "NoForeignActiveBlocked",
            "NoReceiptWithoutTerminalNeedsProof",
            "NoMovedRecordBlocked",
        ],
    },
]


def run_tlc(tla_path: Path, cfg_name: str, metadir: Path):
    cmd = [
        "java",
        "-XX:+UseParallelGC",
        "-cp",
        str(TLA_JAR),
        "tlc2.TLC",
        "-workers",
        "1",
        "-metadir",
        str(metadir),
        "-config",
        cfg_name,
        tla_path.name,
    ]
    res = subprocess.run(cmd, cwd=tla_path.parent, capture_output=True, text=True)
    return res.returncode, res.stdout + res.stderr


def parse_counts(output: str):
    match = re.search(
        r"(\d+) states generated, (\d+) distinct states found, 0 states left on queue\.",
        output,
    )
    if not match:
        return None
    return {"generated": int(match.group(1)), "distinct": int(match.group(2))}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    root = args.root.resolve()
    summary = []
    with tempfile.TemporaryDirectory(prefix="macops-tlc-verify-", dir="/private/tmp") as tmp:
        tmp_dir = Path(tmp)
        for item in MODELS:
            tla = root / item["tla"]
            cfg = root / item["cfg"]
            code, out = run_tlc(tla, cfg.name, tmp_dir / item["name"])
            if code != 0 or "No error has been found." not in out:
                raise SystemExit(f"TLC failed on {item['name']} (exit {code}):\n{out}")
            counts = parse_counts(out)
            witness_results = {}
            for witness in item["witnesses"]:
                w_dir = tmp_dir / f"run-{item['name']}-{witness}"
                w_dir.mkdir(parents=True, exist_ok=True)
                w_tla = w_dir / tla.name
                w_tla.write_text(tla.read_text())
                w_cfg = w_dir / f"{witness}.cfg"
                witness_base = cfg.read_text() if item.get("witness_cfg_from_baseline") else item.get("constants", "") + "SPECIFICATION Spec\n"
                w_cfg.write_text(witness_base + f"\nINVARIANT {witness}\n")
                w_code, w_out = run_tlc(w_tla, w_cfg.name, tmp_dir / f"meta-{item['name']}-{witness}")
                if w_code != 12 or f"Invariant {witness} is violated." not in w_out:
                    raise SystemExit(
                        f"Witness {witness} in {item['name']} was vacuous (exit {w_code}):\n{w_out}"
                    )
                witness_results[witness] = "reachable (exit 12)"
            summary.append(
                {
                    "model": item["name"],
                    "status": "pass",
                    "states": counts,
                    "witnesses": witness_results,
                }
            )
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
