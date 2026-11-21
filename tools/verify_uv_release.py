"""Run the absolute UV-RO release artifact against synthetic roots and access canaries."""
import argparse,hashlib,json,os,pathlib,stat,subprocess,tempfile,time
BASE=pathlib.Path(__file__).resolve().parent.parent
ART=BASE/'artifacts/uv-readonly/release-acceptance';ART.mkdir(parents=True,exist_ok=True)
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',required=True,type=pathlib.Path)
BINARY=parser.parse_args().binary.resolve(strict=True)
LIB=ART/'open_canary.dylib'
subprocess.run(['/usr/bin/clang','-Wall','-Wextra','-Werror','-dynamiclib',str(BASE/'tools/uv-probes/open_canary.c'),'-o',str(LIB)],check=True)
def snapshot(root):
 result={}
 for path in [root,*sorted(root.rglob('*'))]:
  meta=path.lstat();result[str(path.relative_to(root))]=(meta.st_mode,meta.st_mtime_ns,os.readlink(path) if path.is_symlink() else hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None)
 return result
with tempfile.TemporaryDirectory(prefix='uv-release-',dir='/private/tmp') as temporary:
 root=pathlib.Path(temporary);home=root/'home';home.mkdir();(home/'bin').mkdir()
 catalog=root/'catalog.json';state=root/'state';cache=root/'cache';events=[]
 a=home/'A/tools';b=home/'B/tools';ab=home/'A/bin';bb=home/'B/bin'
 for tool,bindir,version in [(a,ab,'1.0'),(b,bb,'2.0')]:
  env=tool/'demo-tool';dist=env/f'lib/python3.12/site-packages/demo_tool-{version}.dist-info';dist.mkdir(parents=True);(env/'bin').mkdir();bindir.mkdir(parents=True)
  (dist/'METADATA').write_text(f'Name: demo-tool\nVersion: {version}\n');(env/'pyvenv.cfg').write_text('home = /synthetic-python\n')
  (env/'uv-receipt.toml').write_text(f"[tool]\nrequirements=[{{name='demo-tool'}}]\nentrypoints=[{{name='demo-tool',install-path='{bindir}/demo-tool'}}]\n")
  (env/'bin/demo-tool').write_text(f'#!/bin/sh\nprintf entry >> \"{home}/native-called\"\nexit 99\n');(env/'bin/demo-tool').chmod(0o755);(bindir/'demo-tool').symlink_to(env/'bin/demo-tool');(env/'bin/python').symlink_to('/synthetic-python')
 for executable in ['uv','python','python3','keyring']:
  path=home/'bin'/executable;path.write_text(f'#!/bin/sh\nprintf called >> "{home}/native-called"\nexit 99\n');path.chmod(0o755)
 (home/'.netrc').write_text('machine synthetic login CANARY password SYNTHETIC\n');credentials=home/'uv-credentials';credentials.write_text('SYNTHETIC-UV-CREDENTIALS')
 catalog.write_text(json.dumps({'schema_version':1,'packages':[{'id':f'owner:{name}','name':'demo-tool','kind':'cli','official_url':'https://example.org/demo','provider':{'type':'uv','package':'Demo_Tool','binding':{'tool_dir':str(tool),'bin_dir':str(bindir)}}} for name,tool,bindir in [('a',a,ab),('b',b,bb)]]}))
 def run(args,expected=0,canary=None):
  env=os.environ.copy();env.update(HOME='/synthetic-ambient-home',PATH=str(home/'bin')+':/usr/bin:/bin')
  log=ART/f'open-{len(events)}.log'
  if canary:
   if log.exists():log.unlink()
   env.update(DYLD_INSERT_LIBRARIES=str(LIB),MACOPS_UV_CANARY=str(canary),MACOPS_UV_OPEN_LOG=str(log))
  start=time.monotonic();output=subprocess.run([str(BINARY),'--json','--home',str(home),'--state-dir',str(state),'--cache-dir',str(cache),'--catalog',str(catalog),*args],env=env,capture_output=True,text=True,timeout=4)
  value=json.loads(output.stdout);elapsed=time.monotonic()-start
  events.append({'args':args,'code':output.returncode,'seconds':elapsed,'value':value})
  assert output.returncode==expected,(args,output.returncode,value,output.stderr)
  if canary:assert log.exists() and log.read_bytes()==b'',(args,'opened-canary',log.read_text() if log.exists() else 'missing trace')
  return value['data'] if expected==0 else value
 before=snapshot(home)
 data=run(['list','--kind','applications'],canary=home/'.netrc');rows=[r for r in data['resources'] if r.get('manager')=='uv'];assert len(rows)==2
 ids={r['scope']:r['id'] for r in rows};assert ids[str(a)]!=ids[str(b)];id_a=ids[str(a)]
 for arguments in [['info',id_a],['doctor','owner:a'],['--dry-run','adopt',id_a]]:run(arguments,canary=credentials)
 assert snapshot(home)==before and not state.exists() and not cache.exists()
 adopted=run(['adopt',id_a,'--yes']);assert adopted['phase']=='succeeded' and adopted['plan']['command'] is None and adopted['plan']['target'] is None
 assert adopted['plan']['package']['id']=='owner:a' and 'authority' not in adopted['plan']['package']['provider']
 receipt=(state/'receipts.json').read_bytes();op=adopted['id'];state_before=snapshot(state)
 for action in ['install','upgrade','uninstall','update']:
  arguments=[action,id_a]+(['--version','9.0'] if action in ['install','upgrade'] else [])
  value=run(arguments,3,home/'.netrc');assert value['error']['code']=='uv_write_blocked_review'
 assert snapshot(home)==before and snapshot(state)==state_before and not cache.exists()
 pending=dict(adopted,phase='prepared');(state/'active.json').write_text(json.dumps(pending));(state/'operations'/f'{op}.json').write_text(json.dumps(pending))
 for _ in range(2):assert run(['recover',op])['phase']=='succeeded'
 assert (state/'receipts.json').read_bytes()==receipt and snapshot(home)==before
 worker_log=ART/'worker-open.log'
 worker_env=os.environ.copy();worker_env.update(DYLD_INSERT_LIBRARIES=str(LIB),MACOPS_UV_CANARY=str(home/'.netrc'),MACOPS_UV_OPEN_LOG=str(worker_log))
 worker=subprocess.run([str(BINARY),'--json','__worker',str(state/'operations'/f'{op}.json')],input='RUN\n',env=worker_env,capture_output=True,text=True,timeout=4)
 assert worker.returncode==3 and 'uv_write_blocked_review' in worker.stdout+worker.stderr,(worker.returncode,worker.stdout,worker.stderr)
 assert worker_log.exists() and worker_log.read_bytes()==b''
 events.append({'args':['__worker',op],'code':worker.returncode,'native_runs':0})
 # Construct the original typed legacy wire shape, preserving Rust field order
 # for its existing SHA contract. Only synthetic journal files are changed.
 def hash_json(value):return hashlib.sha256(json.dumps(value,separators=(',',':'),ensure_ascii=False).encode()).hexdigest()
 legacy_package={'id':'legacy:uv','name':'demo-tool','kind':'cli','official_url':'https://example.org/demo','provider':{'type':'uv','package':'demo-tool'},'paths':[],'exact_version_required':False}
 command={'program':str(home/'bin/uv'),'args':['tool','list']}
 legacy_plan={'timeout_secs':120,'package':legacy_package,'action':'upgrade','before':{'status':'present','version':'1.0'},'target':'9.0','command':{'program':str(home/'bin/uv'),'args':['tool','install','demo-tool==9.0']},'query':command,'recipe_sha256':hash_json(legacy_package),'warnings':[],'synchronous':True,'delegated':False}
 legacy=dict(adopted,id='OP-LEGACY',phase='unknown',process_group=None,plan=legacy_plan,plan_sha256=hash_json(legacy_plan))
 (state/'active.json').write_text(json.dumps(legacy));(state/'operations/OP-LEGACY.json').write_text(json.dumps(legacy));legacy_before=snapshot(state)
 legacy_result=run(['recover','OP-LEGACY'],3,home/'.netrc');assert legacy_result['data']['reason']=='legacy_uv_query_unbound'
 assert snapshot(state)==legacy_before and (state/'receipts.json').read_bytes()==receipt
 for relative in ['demo-tool/uv-receipt.toml','demo-tool/pyvenv.cfg','demo-tool/lib/python3.12/site-packages/demo_tool-1.0.dist-info/METADATA','demo-tool/bin/demo-tool','.lock']:
  path=a/relative;original=path.read_bytes() if path.exists() else None
  if path.exists():path.unlink()
  for kind in ['symlink','fifo','directory']:
   if kind=='symlink':path.symlink_to(home/'.netrc')
   elif kind=='fifo':os.mkfifo(path)
   else:path.mkdir()
   value=run(['doctor','owner:a'],3,home/'.netrc');assert value['data'][0]['observation']['status']=='unknown'
   if kind=='directory':path.rmdir()
   else:path.unlink()
  if original is not None:path.write_bytes(original)
 old_root=home/'old-A-root';a.rename(old_root);a.mkdir();(old_root/'demo-tool').rename(a/'demo-tool')
 blocked=run(['--dry-run','adopt',id_a],3,home/'.netrc');assert blocked['error']['code'] in ['uv_observation_unknown','uv_observation_required']
 assert not (home/'native-called').exists() and not (a/'.lock').exists() and not (b/'.lock').exists()
 (ART/'results.json').write_text(json.dumps({'binary':str(BINARY),'sha256':hashlib.sha256(BINARY.read_bytes()).hexdigest(),'events':events,'native_calls':0,'state_and_cache_readonly_before_adopt':True,'payload_bin_and_credential_trees_unchanged_by_read_or_adopt':True},ensure_ascii=False,indent=2)+'\n')
 print('PASS',len(events),'release invocations; zero native calls; 15 leaf-kind Unknown/access/bounded cases; read/adopt payload tree unchanged')
