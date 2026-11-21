import pathlib
exec((pathlib.Path(__file__).parent/'verify_uv_witnesses.py').read_text().split("for name in")[0])
s=(model/'UvObserved.tla').read_text()
mutants=[('native-write','UvNativeRunsAlwaysZero',s.replace(' \\/ request="write"','').replace('nativeRuns,stateWrites,macopsBChanges,recovered,lock>>\nPreview', 'stateWrites,macopsBChanges,recovered,lock>>\n /\\ nativeRuns\'=IF request="write" THEN 1 ELSE 0\nPreview')),
('owner-early-return','NoSecondOwner',s.replace('Cardinality(owners)>1 \\/ ', '').replace(' /\\ Cardinality(owners)<=1','').replace('owners\'=(IF phase\'="committed" THEN {"owner"} ELSE owners)','owners\'=owners')),
('home-identity','SameCanonicalIdentityAcrossViews',s.replace('<<"uv", r, "demo-tool">>','<<"uv", "HOME", "demo-tool">>')),
('missing-adopt','AdoptOnlyStablePresent',s.replace('observation="present" /\\ ','')),
('stamp-omitted','DriftBeforeCommitBlocksReceipt',s.replace(' /\\ stamp=frozen','').replace('IF stamp=frozen /\\ ','IF ')),
('recover-spawn','RecoverNeverSpawns',s.replace(' /\\ phase\'=(IF receiptOp=journalOp'," /\\ nativeRuns'=1\n /\\ phase'=(IF receiptOp=journalOp").replace('journalOp,receiptOp,nativeRuns,stateWrites,macopsBChanges,lock>>','journalOp,receiptOp,stateWrites,macopsBChanges,lock>>')),
('recover-new-operation','CommitUsesOriginalOperation',s.replace("Recover == /\\ phase=\"crashed\" /\\ recovered'=TRUE", "Recover == /\\ phase=\"crashed\" /\\ recovered'=TRUE /\\ journalOp'=2").replace('observation,journalOp,receiptOp,nativeRuns,stateWrites,macopsBChanges,lock>>','observation,receiptOp,nativeRuns,stateWrites,macopsBChanges,lock>>'))]
for name,inv,ms in mutants:
 with tempfile.TemporaryDirectory(prefix='uv-mutant-',dir='/private/tmp') as temp:
  ms=ms.split('UvNativeRunsAlwaysZero ==')[0]+'UvNativeRunsAlwaysZero =='+s.split('UvNativeRunsAlwaysZero ==')[1]
  d=pathlib.Path(temp);(d/'UvObserved.tla').write_text(ms);(d/'m.cfg').write_text('SPECIFICATION Spec\nCHECK_DEADLOCK FALSE\nINVARIANT '+inv+'\n')
  r=subprocess.run(['java','-XX:+UseParallelGC','-cp',jar,'tlc2.TLC','-workers','1','-metadir',str(d/'states'),'-config','m.cfg','UvObserved.tla'],cwd=d,capture_output=True,text=True)
  (art/('mutant-'+name+'.log')).write_text(r.stdout+r.stderr);(art/('mutant-'+name+'.tla')).write_text(ms)
  assert r.returncode!=0 and 'Invariant '+inv+' is violated' in r.stdout, (name,r.stdout,r.stderr)
  print(name,'VALID KILLED')
