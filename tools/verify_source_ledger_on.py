#!/usr/bin/env python3
"""Check frozen Source Upgrade O/N ledgers and exact raw-oracle counterexamples."""
import os
import argparse,json,subprocess,tempfile
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--root',type=Path,default=Path(__file__).resolve().parents[1])
p.add_argument('--output',type=Path)
p.add_argument('--jar',type=Path,default=Path(os.environ.get("TLA2TOOLS_JAR", "tla2tools.jar")).resolve())
a=p.parse_args();root=a.root.resolve();model=root/'docs/models/source_upgrade'
source=(model/'SourceLedgerON.tla').read_text();config=(model/'SourceLedgerON.cfg').read_text();logs=a.output or root/'artifacts/source-delivery/ledger-on-validation';logs.mkdir(parents=True,exist_ok=True);results=[]
def change(needle,replacement,body=source):
    if body.count(needle)!=1:raise RuntimeError('nonunique anchor: '+needle)
    return body.replace(needle,replacement)
def injected(action):return change('Next == Progress',action+'\nNext == Unsafe \\/ Progress')
with tempfile.TemporaryDirectory(prefix='macops-ledger-on-model-',dir='/private/tmp') as tmp:
    def run(name,body,cfg,code,text):
        d=Path(tmp)/name;d.mkdir();(d/'SourceLedgerON.tla').write_text(body);(d/'SourceLedgerON.cfg').write_text(cfg)
        r=subprocess.run(['java','-XX:+UseParallelGC','-cp',str(a.jar),'tlc2.TLC','-workers','1','-metadir',str(d/'states'),'-config','SourceLedgerON.cfg','SourceLedgerON.tla'],cwd=d,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
        (logs/(name+'.log')).write_text(r.stdout);row={'name':name,'exit':r.returncode,'expected_exit':code,'expected':text,'matched':r.returncode==code and text in r.stdout};results.append(row);print(json.dumps(row),flush=True)
    def invariant(name,body,oracle,cfg=config):run(name,body,cfg,12,'Invariant '+oracle+' is violated.')
    run('baseline',source,config,0,'No error has been found.')
    witnesses=['NoForwardExistingTarget','NoForwardExistingStage','NoForwardMissingTarget','NoForwardMissingStage','NoReverseExistingTarget','NoReverseExistingStage','NoReverseMissingTarget','NoReverseMissingStage','NoActualRollbackClearGap','NoTwoCrashesAfterReverse','NoCompleteAfterClearGap','NoRetainedDeleteBeforeClear','NoRetainedDeleteAfterClear','NoEvidenceLossBlocked','NoPathCollisionRejected','NoIdentityAliasRejected','NoCrossVolumeRejected','NoBudgetRejected','NoParentRejected','NoRootOverlapRejected']
    for witness in witnesses:invariant('witness-'+witness,source,witness,'SPECIFICATION Spec\nINVARIANT '+witness+'\n')
    bad=change('!.objects[s.locations[<<k,"staged">>]]=IF O(k)=Nil THEN IF At(k,"target")=Nil THEN Nil ELSE @ ELSE At(k,"target")','!.objects[s.locations[<<k,"staged">>]]=Nil')
    invariant('mutant-overwrite-forward-rename',bad,'ProductPreservesObjects')
    bad=change('!.objects[s.locations[<<k,"staged">>]]=IF O(k)=Nil /\\ At(k,"staged")#Nil THEN @ ELSE At(k,"target")','!.objects[s.locations[<<k,"staged">>]]=IF O(k)=Nil THEN @ ELSE At(k,"target")')
    invariant('mutant-missing-reverse-unlink',bad,'ProductPreservesObjects')
    bad=injected('Unsafe == AllAfter /\\ s.pending /\\ s\'=[s EXCEPT !.objects[s.locations[<<1,"staged">>]]=Nil]')
    invariant('mutant-cleanup-retained-old',bad,'ProductPreservesObjects')
    start=source.index('Before(k) ==');end=source.index('Known(k) ==',start)
    bad=source[:start]+'Before(k) == At(k,"target")=O(k)\nAfter(k) == IF k \\in Changed(s.case) THEN At(k,"target")=N(k) ELSE Before(k)\n'+source[end:]
    invariant('mutant-target-only-classification',bad,'RollbackComplete')
    bad=change('Before(k) == At(k,"target")=O(k) /\\','Before(k) == (At(k,"target")=O(k) \\/ At(k,"target")=F(s.locations[<<k,"target">>])) /\\')
    invariant('mutant-foreign-same-bytes-before',bad,'RollbackComplete')
    bad=injected('Unsafe == s.phase="recover" /\\ Safe /\\ (\\E k \\in s.reversed:Before(k) /\\ s\'=[s EXCEPT !.objects[s.locations[<<k,"target">>]]=At(k,"staged"),!.objects[s.locations[<<k,"staged">>]]=At(k,"target")])')
    invariant('mutant-repeat-reverse',bad,'ReverseStaysBefore')
    for side in ['before','after']:
        bad=change('!.phase="await-'+side+'",!.clearKind="'+side+'"]','!.phase="await-'+side+'",!.clearKind="'+side+'",!.records=[k \\in K |-> Nil]]')
        invariant('mutant-clear-'+side+'-N-evidence',bad,'EvidenceStable')
    bad=injected('Unsafe == s.phase="recover" /\\ (\\E k \\in Changed(s.case):At(k,"staged")=F(s.locations[<<k,"staged">>]) /\\ s\'=[s EXCEPT !.records[k]=At(k,"staged")])')
    invariant('mutant-rebind-N-to-foreign',bad,'EvidenceStable')
    bad=injected('Unsafe == s.phase="recover" /\\ s.lostProof#{} /\\ (\\E k \\in s.lostProof:s\'=[s EXCEPT !.records[k]=N(k)])')
    invariant('mutant-relearn-lost-exact-N',bad,'NeverRelearnLostProof')
    bad=injected('Unsafe == s.phase="recover" /\\ s\'=[s EXCEPT !.createsAfterStart=@+1]')
    invariant('mutant-rebuild-in-recovery',bad,'RecoveryNeverBuilds')
    for skipped in [1,2,3,4]:
        bad=change('(\\A j \\in K:j<k => After(j))','(\\A j \\in K:j<k /\\ j#'+str(skipped)+' => After(j))')
        invariant('mutant-skip-forward-ledger-'+str(skipped),bad,'CommitPrefix')
    bad=change('AllAfter == (\\A k \\in K:After(k))','AllAfter == (\\A k \\in K \\ {5}:After(k))')
    invariant('mutant-skip-receipts',bad,'SuccessComplete')
    bad=change('(\\A j \\in K:j>k => Before(j))','TRUE')
    invariant('mutant-reverse-native-too-early',bad,'CommitPrefix')
    bad=change('/\\ (\\A k \\in K:Before(k)) /\\ ~RootBefore(r)','/\\ ~RootBefore(r)')
    invariant('mutant-restore-root-too-early',bad,'RootsAfterLedgers')
    bad=change('AllBefore == (\\A k \\in K:Before(k)) /\\ (\\A r \\in Roots:RootBefore(r))','AllBefore == (\\A k \\in K:Before(k)) /\\ (\\A r \\in Roots:RootBefore(r) \\/ RootAfter(r))')
    invariant('mutant-skip-root-rollback',bad,'RollbackComplete')
    bad=change('!.records=[k \\in K |-> IF k \\in Changed(s.case) THEN N(k) ELSE Nil]','!.records=[k \\in K |-> Nil]')
    bad=change('JournalStart == s.phase="ready" /\\ CandidateDurable /\\','JournalStart == s.phase="ready" /\\',bad)
    invariant('mutant-journal-before-frozen-N',bad,'EvidenceStable')
    for case in [4,5,6,7,8,9]:
        bad=injected('Unsafe == s.phase="plan" /\\ s.case='+str(case)+' /\\ s\'=[s EXCEPT !.started=TRUE,!.pending=TRUE,!.phase="running"]')
        invariant('mutant-start-invalid-input-'+str(case),bad,'InvalidNeverStarts','SPECIFICATION Spec\nINVARIANT InvalidNeverStarts\n')
(logs/'summary.json').write_text(json.dumps(results,indent=2)+'\n')
raise SystemExit(0 if all(r['matched'] for r in results) else 1)
