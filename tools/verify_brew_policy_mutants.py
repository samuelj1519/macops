#!/usr/bin/env python3
"""Check Brew policy guard regressions with isolated Rust tests."""
from pathlib import Path
import argparse
import hashlib
import json
import re
import shutil
import stat
import subprocess

ROOT=Path(__file__).resolve().parents[1]
POLICY='crates/app/src/software/rules/brew.rs'

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--reuse-build-root',type=Path)
    args=parser.parse_args();args.output.mkdir(parents=True,exist_ok=False)
    files=[]
    for path in ROOT.rglob('*'):
        rel=path.relative_to(ROOT)
        if not path.is_file() or any(p in {'artifacts','target','.git','__pycache__'} for p in rel.parts):continue
        files.append(rel)
    source=(ROOT/POLICY).read_text()
    mutations=[
        ('old-missing-authority','record.epoch == query.context_epoch && record.binding == query.binding','true','rebind_never_blesses_old_missing_even_after_cellar_continuity'),
        ('native-selected-scope','&& native == expected','&& true','every_incomplete_native_proof_is_rejected'),
        ('logical-prefix-omitted','&("brew", &key.canonical_prefix, &key.full_name)','&("brew", "HOME", &key.full_name)','stable_owner_excludes_every_binding_field_and_keeps_prefix_and_native_name'),
    ]
    results=[]
    for name,old,new,test in mutations:
        assert source.count(old)==1,(name,source.count(old))
        work=args.output/name;tree=work/'source';tree.mkdir(parents=True)
        manifest={}
        for rel in files:
            path=ROOT/rel;dest=tree/rel;dest.parent.mkdir(parents=True,exist_ok=True)
            shutil.copyfile(path,dest);dest.chmod(stat.S_IMODE(path.stat().st_mode))
            manifest[str(rel)]={'sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'mode':stat.S_IMODE(path.stat().st_mode)}
        (work/'source-base.json').write_text(json.dumps(manifest,sort_keys=True,indent=2)+'\n')
        (tree/POLICY).write_text(source.replace(old,new,1))
        build=(args.reuse_build_root/name/'build') if args.reuse_build_root else (work/'build')
        command=['/usr/bin/env','RUSTC_WRAPPER=','CARGO_TARGET_DIR='+str(build),'cargo','test','--workspace','--lib','software::rules::brew::tests::'+test,'--all-features','--locked','--offline','--','--exact','--nocapture']
        result=subprocess.run(command,cwd=tree,capture_output=True,text=True,timeout=240)
        output=result.stdout+result.stderr;(work/'test.log').write_text(output)
        valid=result.returncode==101 and 'test result: FAILED' in output and '1 failed' in output and 'could not compile' not in output and 'error[E' not in output
        matches=re.findall(r'Running unittests src/lib.rs \(([^)]+)\)',output)
        assert matches,(name,'no actual executed test binary')
        binary=Path(matches[-1]);binary=binary if binary.is_absolute() else tree/binary
        assert binary.is_file(),str(binary)
        item={'actual_test_binary':str(binary),'actual_test_binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'base_policy_sha256':hashlib.sha256(source.encode()).hexdigest(),'name':name,'command':command,'exit':result.returncode,'valid_compiled_kill':valid,'source_sha256':hashlib.sha256((tree/POLICY).read_bytes()).hexdigest(),'expected_test':test}
        results.append(item);(args.output/'results.json').write_text(json.dumps(results,indent=2)+'\n')
        print(json.dumps(item),flush=True)
        assert valid,(name,output[-6000:])

if __name__=='__main__':main()
