#!/usr/bin/env python3
"""Compile Brew writer faults against isolated Native fixtures; never run installed Brew."""
from pathlib import Path
import argparse
import hashlib
import json
import re
import shutil
import stat
import subprocess

ROOT=Path(__file__).resolve().parents[1]
POLICY='crates/app/src/software/brew.rs'

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
    start=source.index('        if doctor {',source.index('fn current_context('))
    end=source.index('        register_context(',start)
    old_trust=source[start:end]
    replay='    let _replayed = std::process::Command::new(&binding.invocation.command.program)\n        .args(&binding.invocation.command.args).env_clear().envs(&binding.invocation.env)\n        .current_dir(&binding.invocation.cwd).status()?;\n'
    original_finish='    finish_native(ctx, store, &mut journal, &binding)\n}\n\n#[cfg(test)]'
    mutations=[
        ('unconfirmed-context-trust',old_trust,'','another_registry_does_not_trust_a_new_context_and_first_explicit_action_confirms'),
        ('wrong-target-accepted','if !policy::expected(&view, &proof.observation) {','if false && !policy::expected(&view, &proof.observation) {','original_nonzero_and_bad_after_remain_unknown_without_native_retry'),
        ('recover-replays-mutator',original_finish,replay+original_finish,'original_nonzero_and_bad_after_remain_unknown_without_native_retry'),
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
        command=['/usr/bin/env','RUSTC_WRAPPER=','CARGO_TARGET_DIR='+str(build),'cargo','test','--workspace','--test','cli','brew_native::'+test,'--all-features','--locked','--offline','--','--exact','--nocapture']
        result=subprocess.run(command,cwd=tree,capture_output=True,text=True,timeout=240)
        output=result.stdout+result.stderr;(work/'test.log').write_text(output)
        valid=result.returncode==101 and 'test result: FAILED' in output and '1 failed' in output and 'could not compile' not in output and 'error[E' not in output
        matches=re.findall(r'Running tests/cli.rs \(([^)]+)\)',output)
        assert matches,(name,'no actual executed test binary')
        binary=Path(matches[-1]);binary=binary if binary.is_absolute() else tree/binary
        assert binary.is_file(),str(binary)
        item={'actual_test_binary':str(binary),'actual_test_binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'base_brew_sha256':hashlib.sha256(source.encode()).hexdigest(),'name':name,'command':command,'exit':result.returncode,'valid_compiled_kill':valid,'source_sha256':hashlib.sha256((tree/POLICY).read_bytes()).hexdigest(),'expected_test':test}
        results.append(item);(args.output/'results.json').write_text(json.dumps(results,indent=2)+'\n')
        print(json.dumps(item),flush=True)
        assert valid,(name,output[-6000:])

if __name__=='__main__':main()
