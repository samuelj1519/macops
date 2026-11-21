"""Verify fixed Native Skills stages, witnesses, and guard mutants with existing TLC."""
import os
import json
import pathlib
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
MODEL = ROOT / "docs/models/native_skills"
ART = ROOT / "artifacts/native-skills/model-checks"
JAVA = os.environ.get("JAVA", "java")
JAR = str(pathlib.Path(os.environ.get("TLA2TOOLS_JAR", "tla2tools.jar")).resolve())
ART.mkdir(parents=True, exist_ok=True)
SOURCE = (MODEL / "NativeSkills.tla").read_text()
CONFIG = (MODEL / "NativeSkills.cfg").read_text()
resume = sys.argv[1] if len(sys.argv) > 1 else None
active = resume is None
results = json.loads((ART / "results.json").read_text()) if resume else []


def run(name, source, config, violation=None):
    global active
    if not active:
        if name != resume:
            return
        active = True
    with tempfile.TemporaryDirectory(prefix="native-skills-model-", dir="/private/tmp") as tmp:
        directory = pathlib.Path(tmp)
        (directory / "NativeSkills.tla").write_text(source)
        (directory / "check.cfg").write_text(config)
        command = [JAVA, "-XX:+UseParallelGC", "-cp", JAR, "tlc2.TLC", "-workers", "1", "-metadir", str(directory / "states"), "-config", "check.cfg", "NativeSkills.tla"]
        result = subprocess.run(command, cwd=directory, text=True, capture_output=True, timeout=60)
        output = result.stdout + result.stderr
        (ART / (name + ".log")).write_text(output)
        (ART / (name + ".tla")).write_text(source)
        (ART / (name + ".cfg")).write_text(config)
        expected = "No error has been found" if violation is None else ("Action property " + violation[7:] + " is violated" if violation.startswith("ACTION:") else "Invariant " + violation + " is violated")
        expected_exit = 0 if violation is None else (13 if violation.startswith("ACTION:") else 12)
        valid = result.returncode == expected_exit and expected in output
        results.append({"name": name, "exit": result.returncode, "valid": valid, "expectedViolation": violation})
        (ART / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        if not valid:
            raise RuntimeError(name + " invalid probe: " + output)
        print(name, "PASS" if violation is None else "EXPECTED VIOLATION", flush=True)


run("baseline", SOURCE, CONFIG)
run("dry-run", SOURCE, CONFIG.replace('Mode = "write"', 'Mode = "dry"'))
for name, old, new in [("remove-unshared", 'Action = "install"', 'Action = "remove"'), ("bad-registry", 'RegistryValid = TRUE', 'RegistryValid = FALSE')]:
    run(name, SOURCE, CONFIG.replace(old, new))
for references in ["other", "unknown"]:
    run("remove-reference-" + references, SOURCE, CONFIG.replace('Action = "install"', 'Action = "remove"').replace('References = "none"', 'References = "' + references + '"'))
derived_config = CONFIG.replace('Delivery = "direct"', 'Delivery = "derived"').replace('Steps = 2', 'Steps = 0')
license_config = CONFIG.replace('Steps = 2', 'Steps = 4').replace('Already = {}', 'Already = {3}')
run("direct-no-customization", SOURCE, CONFIG.replace('Steps = 2', 'Steps = 0'))
run("license-append-and-noop", SOURCE, license_config)
run("derived-artifact", SOURCE, derived_config)
run("dry-derived-no-artifact", SOURCE, derived_config.replace('Mode = "write"', 'Mode = "dry"'))
for witness, cfg in [("NoArtifactReady", derived_config), ("NoArtifactFailure", derived_config), ("NoDerivedUpgradeSameSlot", derived_config), ("NoSameContentNoop", license_config), ("NoLicenseConflictBlocked", license_config)]:
    run("witness-" + witness, SOURCE, cfg + "INVARIANT " + witness + "\n", witness)
for witness in ["NoSuccess", "NoPartial", "NoUnknownAfterCrash", "NoUnknownWithLivePayload", "NoRecoveryAfterFirstPatch", "NoForeignBlocked"]:
    run("witness-" + witness, SOURCE, CONFIG + "INVARIANT " + witness + "\n", witness)

def native_lock_mutant(source):
    start = source.index("Start ==")
    end = source.index("NativeEffect ==", start)
    action = source[start:end].replace("touchedNativeLock,", "")
    action = action.replace("/\\ phase'=", "/\\ touchedNativeLock'=TRUE\n         /\\ phase'=")
    return source[:start] + action + source[end:]


mutants = [
    ("retry-guard-only", "OneNativeCall", SOURCE.replace('         /\\ nativeCalls=0\n', '')),
    ("recovery-respawns", "OneNativeCall", SOURCE.replace('/\\ phase\'=\"CustomizationPending\"\n           /\\ UNCHANGED <<nativeCalls,process', '/\\ phase\'=\"CustomizationPending\"\n           /\\ nativeCalls\'=nativeCalls+1\n           /\\ UNCHANGED <<process')),
    ("overwrite-foreign", "NoForeignOverwrite", SOURCE.replace('/\\ payload=\"Native\" /\\ metadata[n]=\"Before\"', '/\\ payload=\"Native\"')),
    ("early-receipt", "ReceiptRequiresAllStages", SOURCE.replace('/\\ phase=\"CustomizationPending\" /\\ patched=1..Steps\n          /\\ payload=\"Native\" /\\ \\A n \\in 1..Steps: metadata[n]=\"After\"', '/\\ phase=\"CustomizationPending\"')),
    ("ignore-partial-exit", "PartialNeverSucceeds", SOURCE.replace('IF process=\"Success\" /\\ payload=\"Native\"', 'IF payload=\"Native\"')),
    ("query-before-completion", "CustomizationAfterNative", SOURCE.replace('/\\ process \\in {\"Success\",\"Partial\"}\n         /\\ queried', '/\\ process \\in {\"Running\",\"Success\",\"Partial\"}\n         /\\ queried').replace('IF process=\"Success\" /\\ payload=\"Native\"', 'IF payload=\"Native\"')),
    ("native-lock-write", "NativeAndOtherOwnersUntouched", native_lock_mutant(SOURCE)),
]
mutants.append(("other-client-native-change", "OtherClientChangesNotCertified", SOURCE.replace(' /\\ ~otherNativeChange', '')))
for name, invariant, mutant in mutants:
    if mutant == SOURCE:
        raise RuntimeError(name + " mutation did not apply")
    cfg = CONFIG.split('SPECIFICATION')[0] + 'SPECIFICATION Spec\nINVARIANT ' + invariant + '\n'
    run("mutant-" + name, mutant, cfg, invariant)

# The exact same retry guard-only mutant must also violate an independent action property.
retry = SOURCE.replace('         /\\ nativeCalls=0\n', '')
run("mutant-retry-guard-only-action-property", retry, CONFIG.split('SPECIFICATION')[0] + 'SPECIFICATION Spec\nPROPERTY NoRepeat\n', "ACTION:NoRepeat")

extra = [
    ("artifact-ready-guard-only", "DerivedNeedsReadyArtifact", SOURCE.replace('         /\\ (Action=\"remove\" \\/ Delivery=\"direct\" \\/ artifact=\"Ready\")\n', ''), derived_config),
    ("new-source-adds-owner", "StableInstallOwner", SOURCE.replace('receiptIds\'=receiptIds \\cup {\"slot\"}', 'receiptIds\'=receiptIds \\cup {binding}'), derived_config),
    ("stale-receipt-binding", "ReceiptUsesActualSourceBinding", SOURCE.replace("receiptBinding'=binding", "receiptBinding'=receiptBinding"), derived_config),
    ("license-overwrites-foreign", "NoForeignOverwrite", SOURCE.replace('metadata[n]=\"Before\"\n            /\\ metadata\'', '(metadata[n]=\"Before\" \\/ n=3)\n            /\\ metadata\''), license_config),
]
for name, invariant, mutant, cfg in extra:
    if mutant == SOURCE:
        raise RuntimeError(name + " mutation did not apply")
    run("mutant-" + name, mutant, cfg.split('SPECIFICATION')[0] + 'SPECIFICATION Spec\nINVARIANT ' + invariant + '\n', invariant)
