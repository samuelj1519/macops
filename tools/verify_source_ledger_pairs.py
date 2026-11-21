#!/usr/bin/env python3
"""Verify the Source Upgrade file-pair ledger model."""
import os
import argparse,json,subprocess,tempfile
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--root',type=Path,default=Path(__file__).resolve().parents[1])
p.add_argument('--output',type=Path)
p.add_argument('--jar',type=Path,default=Path(os.environ.get("TLA2TOOLS_JAR", "tla2tools.jar")).resolve())
a=p.parse_args();root=a.root.resolve();model=root/'docs/models/source_upgrade'
source=(model/'SourceLedgerPairs.tla').read_text();config=(model/'SourceLedgerPairs.cfg').read_text()
logs=a.output or root/'artifacts/source-delivery/ledger-pair-validation';logs.mkdir(parents=True,exist_ok=True);results=[]
def change(needle,replacement,body=source):
    if body.count(needle)!=1:raise RuntimeError('nonunique anchor: '+needle)
    return body.replace(needle,replacement)
def injected(action):
    return change('Next == Progress',action+'\nNext == Unsafe \\/ Progress')
with tempfile.TemporaryDirectory(prefix='macops-ledger-pair-model-',dir='/private/tmp') as temporary:
    def run(name,body,cfg,code,text):
        d=Path(temporary)/name;d.mkdir();(d/'SourceLedgerPairs.tla').write_text(body);(d/'SourceLedgerPairs.cfg').write_text(cfg)
        cmd=['java','-XX:+UseParallelGC','-cp',str(a.jar),'tlc2.TLC','-workers','1','-metadir',str(d/'states'),'-config','SourceLedgerPairs.cfg','SourceLedgerPairs.tla']
        r=subprocess.run(cmd,cwd=d,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
        (logs/(name+'.log')).write_text(r.stdout)
        row={'name':name,'exit':r.returncode,'expected_exit':code,'expected':text,'matched':r.returncode==code and text in r.stdout};results.append(row);print(json.dumps(row),flush=True)
    run('baseline',source,config,0,'No error has been found.')
    for witness in ['NoForwardForeign','NoRollbackForeign','NoSourceForeign','NoOrphanCrash','NoRestoredCrash','NoClearAfterCrash','NoClearBeforeCrash','NoMissingRoundtrip','NoDestinationBlocked','NoClearMissingCrash','NoRollbackSourceForeign','NoMissingRollbackForeign','NoRollbackDestinationBlocked','NoPathCollisionRejected','NoIdentityCollisionRejected','NoRetainedDeletedBeforeClear','NoRetainedDeletedAfterClear']:
        run('witness-'+witness,source,'SPECIFICATION Spec\nINVARIANT '+witness+'\n',12,'Invariant '+witness+' is violated.')
    bad=change('!.files[<<k,"n">>]=IF k \\in Missing(s.case) THEN IF At(k,"target")=Nil THEN Nil ELSE @ ELSE At(k,"target")','!.files[<<k,"n">>]=Nil')
    run('mutant-overwrite-rename',bad,config,12,'Invariant ProductPreservesObjects is violated.')
    bad=change('!.files[<<k,"n">>]=IF k \\in Missing(s.case) /\\ At(k,"n")=Nil THEN At(k,"target") ELSE @','!.files[<<k,"n">>]=@')
    run('mutant-missing-rollback-unlink',bad,config,12,'Invariant ProductPreservesObjects is violated.')
    start=source.index('Before(k) ==');end=source.index('Known(k) ==',start)
    bad=source[:start]+'''Before(k) == At(k,"target")=Old(k) \\/ (At(k,"target")=R(k) /\\ k \\in s.savedR)
After(k) == IF k \\in Changed(s.case) THEN At(k,"target")=N(k) ELSE Before(k)
'''+source[end:]
    run('mutant-target-only-classification',bad,config,12,'Invariant RawTerminalBefore is violated.')
    bad=injected('Unsafe == s.volatileR#0 /\\ s.phase="recover" /\\ LET k==s.volatileR IN s\'=[s EXCEPT !.files[<<k,"target">>]=R(k),!.files[<<k,"r">>]=N(k),!.volatileR=0]')
    run('mutant-publish-unrecorded-R',bad,config,12,'Invariant RestoredRecorded is violated.')
    bad=injected('Unsafe == s.phase="recover" /\\ Safe /\\ (\\E k \\in K:At(k,"target")=R(k) /\\ s\'=[s EXCEPT !.files[<<k,"target">>]=At(k,"r"),!.files[<<k,"r">>]=At(k,"target")])')
    run('mutant-repeat-restored-exchange',bad,config,12,'Invariant RootAfterLedgerRollback is violated.')
    bad=injected('Unsafe == AllAfter /\\ s.pending /\\ s\'=[s EXCEPT !.files[<<1,"n">>]=Nil]')
    run('mutant-cleanup-retained-O',bad,config,12,'Invariant ProductPreservesObjects is violated.')
    bad=change('!.phase="await-before",!.clearKind="before"]','!.phase="await-before",!.clearKind="before",!.savedR={}]')
    run('mutant-clear-only-R-evidence',bad,'SPECIFICATION Spec\nINVARIANT DurableEvidenceAfterClear\n',12,'Invariant DurableEvidenceAfterClear is violated.')
    bad=change('(\\A j \\in K:j<k => After(j))','(\\A j \\in K:j<k /\\ j#2 => After(j))')
    run('mutant-skip-middle-ledger',bad,config,12,'Invariant CommitPrefix is violated.')
    bad=source.replace('(\\A j \\in K:j>k => Before(j))','TRUE')
    run('mutant-restore-native-too-early',bad,config,12,'Invariant CommitPrefix is violated.')
    bad=injected('Unsafe == s.phase="recover" /\\ s.live="N" /\\ s\'=[s EXCEPT !.live="O",!.retained="N"]')
    run('mutant-root-before-ledger-rollback',bad,config,12,'Invariant RootAfterLedgerRollback is violated.')
    bad=change('s.phase="plan" /\\ PreflightValid /\\ Safe','s.phase="plan" /\\ Safe')
    run('mutant-ignore-path-collision',bad,'SPECIFICATION Spec\nINVARIANT NamespacesSeparated\n',12,'Invariant NamespacesSeparated is violated.')
    run('mutant-ignore-physical-alias',bad,'SPECIFICATION Spec\nINVARIANT UniqueObjects\n',12,'Invariant UniqueObjects is violated.')
    bad=change('!.phase="await-before",!.clearKind="before"]','!.phase="await-before",!.clearKind="before",!.savedMissing={}]')
    run('mutant-clear-Missing-evidence',bad,config,12,'Invariant RollbackEvidenceDurable is violated.')
    bad=change('!.phase="await-before",!.clearKind="before"]','!.phase="await-before",!.clearKind="before",!.evidenceN=FALSE]')
    run('mutant-clear-N-evidence',bad,config,12,'Invariant ClearKeepsN is violated.')
    bad=injected('Unsafe == s.phase="recover" /\\ s.orphanCrash /\\ s.volatileR=0 /\\ (\\E k \\in K:At(k,"r")=R(k) /\\ At(k,"target")=N(k) /\\ At(k,"n")=O(k) /\\ k \\notin s.savedR /\\ s\'=[s EXCEPT !.savedR=@ \\cup {k}])')
    run('mutant-relearn-orphan-R',bad,config,12,'Invariant RProvenance is violated.')
    bad=injected('Unsafe == s.phase="recover" /\\ s.orphaned#{} /\\ (\\E k \\in s.orphaned:s\'=[s EXCEPT !.rCreates[k]=@+1])')
    run('mutant-recreate-orphan-R',bad,config,12,'Invariant OneRCreation is violated.')
(logs/'summary.json').write_text(json.dumps(results,indent=2)+'\n')
raise SystemExit(0 if all(r['matched'] for r in results) else 1)
