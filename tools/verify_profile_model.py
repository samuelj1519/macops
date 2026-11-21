#!/usr/bin/env python3
from pathlib import Path
import os
import tempfile,subprocess,json,re
import argparse
parser=argparse.ArgumentParser(description="Verify ProfileConfig baseline, reachability, and wrong logic.")
parser.add_argument('--root',type=Path,default=Path(__file__).resolve().parents[1])
parser.add_argument('--output',type=Path)
parser.add_argument('--jar',type=Path,default=Path(os.environ.get("TLA2TOOLS_JAR", "tla2tools.jar")).resolve())
args=parser.parse_args()
root=args.root.resolve()
src=root/'docs/models/profile_config'
logs=args.output or root/'artifacts/profile-config/model-validation'
logs.mkdir(parents=True,exist_ok=True)
original=(src/'ProfileConfig.tla').read_text();cfg=(src/'ProfileConfig.cfg').read_text();java=['java','-XX:+UseParallelGC','-cp',str(args.jar),'tlc2.TLC','-workers','1']
base=Path(tempfile.mkdtemp(prefix='macops-owned-model-',dir='/private/tmp'));rows=[]
def run(name,tla,config,expected,expected_exit=12):
 d=base/name;d.mkdir();(d/'ProfileConfig.tla').write_text(tla);(d/'ProfileConfig.cfg').write_text(config)
 p=subprocess.run(java+['-metadir',str(d/'states'),'-config','ProfileConfig.cfg','ProfileConfig.tla'],cwd=d,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
 text=p.stdout.decode();(logs/(name+'.log')).write_text(text)
 valid=p.returncode==expected_exit and expected in text;rows.append({'name':name,'exit':p.returncode,'expected':expected,'matched':valid});print(json.dumps(rows[-1]),flush=True)
run('baseline',original,cfg,'No error has been found.',expected_exit=0)
constants='CONSTANTS\n Missing = "missing"\n External = "external"\nSPECIFICATION Spec\n'
for witness in ['NoSharedOwnerRetirement','NoConflictRetirement','NoMissingParentRestore','NoEmptyParentRestore','NoExternalOwnedBlock','NoChangedUpdate','NoRejectedReplacement','NoSharedAtomicUpdate']:
 run('witness-'+witness,original,constants+'INVARIANT '+witness+'\n','Invariant '+witness+' is violated.')
# Keep the reference captured value fixed while corrupting only the saved original.
bad=original.replace('!.original = IF HasClaims(s.claims) THEN @ ELSE s.current','!.original = s.current')
run('mutant-resample-original',bad,cfg,'Invariant OriginalNotResampled is violated.')
old='!.current = IF HasClaims(next) THEN s.applied ELSE s.original';assert old in original
run('mutant-retire-one-restores-all',original.replace(old,'!.current = s.original'),cfg,'Invariant OtherOwnerKeepsApplied is violated.')
old='(s.originalParent # "missing" \\/ s.original # Missing \\/ s.other)';assert old in original
run('mutant-delete-existing-empty-parent',original.replace(old,'(s.original # Missing \\/ s.other)'),cfg,'Invariant OriginalEmptyParentPreserved is violated.')
run('mutant-remove-unselected-owner',original.replace('!.claims = next, !.retired','!.claims = EmptyClaims, !.retired'),cfg,'Invariant NoForeignOwnerRetirement is violated.')
start=original.index('Reconfigure(selected,d) ==');end=original.index('RejectReconfigure(selected,d) ==',start)
part=original[start:end];old="s' = [s EXCEPT !.claims = NextClaims(selected,d)";assert part.count(old)==1
bad=original[:start]+part.replace(old,"s' = [s EXCEPT !.original = s.current, !.claims = NextClaims(selected,d)")+original[end:]
run('mutant-update-resamples-original',bad,cfg,'Invariant OriginalNotResampled is violated.')
bad=original[:start]+part.replace(old,"s' = [s EXCEPT !.claims = [o \\in Owners |-> IF o \\in selected THEN d ELSE None]")+original[end:]
run('mutant-update-removes-unselected-owner',bad,cfg,'Invariant NoForeignOwnerRetirement is violated.')
start=original.index('RejectReconfigure(selected,d) ==');end=original.index('UserOther ==',start)
part=original[start:end];old="s' = [s EXCEPT !.updated";assert part.count(old)==1
bad=original[:start]+part.replace(old,"s' = [s EXCEPT !.current = d, !.updated")+original[end:]
run('mutant-rejected-update-writes',bad,constants+'INVARIANT NoWriteOnRejectedReplacement\n','Invariant NoWriteOnRejectedReplacement is violated.')
(logs/'summary.json').write_text(json.dumps(rows,indent=2)+'\n');assert all(r['matched'] for r in rows)
