"""Compile UV FD safeguards and three valid mutants against actual access oracles."""
import os
import pathlib
import shutil
import subprocess
import tempfile

BASE=pathlib.Path(__file__).resolve().parent.parent
ART=BASE/'artifacts/uv-readonly/io-mutants'
ART.mkdir(parents=True,exist_ok=True)
LIB=ART/'open_canary.dylib'
subprocess.run(['/usr/bin/clang','-Wall','-Wextra','-Werror','-dynamiclib',str(BASE/'tools/uv-probes/open_canary.c'),'-o',str(LIB)],check=True)
CANARY=ART/'synthetic-canary'
CANARY.write_text('SYNTHETIC-CREDENTIAL-CANARY')
original=(BASE/'crates/app/src/software/uv_local.rs').read_text()
mutants=[('baseline',original,'leaf_canary_is_never_opened_and_races_use_original_fd'),
 ('nofollow',original.replace('OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC','OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC'),'leaf_canary_is_never_opened_and_races_use_original_fd'),
 ('reopen',original.replace('(&file)\n            .take(size.saturating_add(1))','File::open(self.path.join(name))?\n            .take(size.saturating_add(1))'),'leaf_canary_is_never_opened_and_races_use_original_fd'),
 ('file-type',original.replace('if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {\n            return Err(unknown());\n        }',''),'leaf_rejects_symlink_fifo_and_directory')]
for name,source,test in mutants:
 if name!='baseline':assert source!=original,name
 with tempfile.TemporaryDirectory(prefix='uv-io-'+name+'-',dir='/private/tmp') as directory:
  root=pathlib.Path(directory)
  shutil.copytree(BASE,root,dirs_exist_ok=True,ignore=shutil.ignore_patterns('.git','target','artifacts','__pycache__'))
  (root/'crates/app/src/software/uv_local.rs').write_text(source)
  (ART/(name+'.rs')).write_text(source)
  log=ART/(name+'-open.log')
  if log.exists():log.unlink()
  env=os.environ.copy();env.update(RUSTC_WRAPPER='',CARGO_TARGET_DIR='/private/tmp/macops-build-uv-io-mutants',MACOPS_UV_RACE_CANARY=str(CANARY),MACOPS_UV_CANARY=str(CANARY),MACOPS_UV_OPEN_LOG=str(log),DYLD_INSERT_LIBRARIES=str(LIB))
  build_env=env.copy();build_env.pop('DYLD_INSERT_LIBRARIES')
  built=subprocess.run(['cargo','test','--locked','--offline','--lib','--no-run','--message-format=json'],cwd=root,env=build_env,capture_output=True,text=True,timeout=180)
  import json
  artifacts=[json.loads(line) for line in built.stdout.splitlines() if line.startswith('{')]
  binaries=[row['executable'] for row in artifacts if row.get('reason')=='compiler-artifact' and row.get('executable') and row.get('profile',{}).get('test')]
  assert built.returncode==0 and len(binaries)==1,(name,built.stdout,built.stderr)
  result=subprocess.run([binaries[0],'software::uv_local::tests::'+test,'--exact'],cwd=root,env=env,capture_output=True,text=True,timeout=10)
  output=result.stdout+result.stderr
  (ART/(name+'.log')).write_text(built.stderr+'\n'+output)
  assert 'running 1 test' in output and log.exists(),(name,output)
  opened=log.read_text()
  if name=='baseline':assert result.returncode==0 and opened=='',(output,opened)
  elif name=='file-type':assert result.returncode!=0 and 'FAILED' in output,output
  else:assert 'OPENED-CANARY' in opened,(name,output,opened)
  print(name,'PASS' if name=='baseline' else 'VALID KILLED',flush=True)
