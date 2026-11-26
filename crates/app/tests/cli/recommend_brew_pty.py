import errno,json,os,pathlib,pty,re,select,subprocess,sys,time
q=json.load(open(sys.argv[1])); master,slave=pty.openpty()
if 'inventory' in q:
 import fcntl,struct,termios
 fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 28, 120, 0, 0))
p=subprocess.Popen(q['argv'],stdin=slave,stdout=slave,stderr=slave,env=q['env'])
os.close(slave)
buf=bytearray(); deadline=time.monotonic()+30
def read_until(phrase):
 def contains():
  value=re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]',b'',bytes(buf))
  return b''.join(phrase.encode().split()) in b''.join(value.split())
 while not contains() and time.monotonic()<deadline:
  if select.select([master],[],[],.25)[0]:
   try: data=os.read(master,65536)
   except OSError as e:
    if e.errno == errno.EIO: break
    raise
   if not data: break
   buf.extend(data)
 assert contains(),buf.decode(errors='replace')
try:
 if q.get('mode') == 'batch-discovery-cancel':
  read_until('首次信任此具体Native程序及查询范围')
  read_until('确认执行')
  os.write(master, b'n\n')
  read_until('已取消')
  assert p.wait(timeout=10) == 3
  assert not pathlib.Path(q['events']).exists()
  sys.exit(0)
 if 'inventory' in q:
  key, result = q['inventory']
  read_until('本机盘点')
  os.write(master,b'/foo\r\r')
  read_until('u 升级')
  os.write(master,key.encode())
  read_until('确认已核验的Native作用范围')
  read_until('确认执行')
  assert b'foo' in buf and b'native-cellar' in buf
  assert not pathlib.Path(q['mutated']).exists()
  os.write(master,b'\x1b[B\r' if q['approved'] else b'\r')
  read_until(result)
  os.write(master,b'q')
  assert p.wait(timeout=10) in (0,3),buf.decode(errors='replace')
  assert b'private-fixture-token' not in buf
  sys.exit(0)
 read_until('选择安装候选')
 assert b'Homebrew' in buf
 assert not pathlib.Path(q['events']).exists()
 if q['mode'] == 'pending-cancel':
  os.write(master,b'1\n')
  read_until('上一次操作')
  read_until('再准备本次计划')
  os.write(master,b'n\n')
  read_until('已取消')
  assert p.wait(timeout=10)==3
  assert not pathlib.Path(q['events']).exists()
  sys.exit(0)
 if q['mode'] in ('stale','trust-stale'):
  if q['mode']=='trust-stale':
   os.write(master,b'1\n')
   read_until('确认执行')
  path=pathlib.Path(q['catalog'])
  catalog=json.loads(path.read_text())
  catalog['packages'][0]['provider']['recipe_ref']='changed-after-selection'
  path.write_text(json.dumps(catalog))
  os.write(master,b'\x1b[B\r' if q['mode']=='trust-stale' else b'1\n')
  read_until('plan_stale')
  assert p.wait(timeout=10)==3
  assert not pathlib.Path(q['events']).exists()
  sys.exit(0)
 os.write(master,b'1\n')
 read_until('确认执行')
 assert not pathlib.Path(q['mutated']).exists()
 events=pathlib.Path(q['events']).read_text().splitlines()
 assert len(events)==1 and json.loads(events[0])[0]=='ruby',events
 os.write(master,b'\x1b[B\r' if q['approved'] else b'\r')
 if q['approved']:
  read_until('fixture brew stdout progress')
  read_until('fixture brew stderr progress')
  mutated=pathlib.Path(q['mutated'])
  assert not mutated.exists()
  mutated.with_name('progress-release').touch()
  read_until('结果 已完成')
 assert p.wait(timeout=10)==(0 if q['approved'] else 3)
 assert b'private-fixture-token' not in buf
finally:
 if p.poll() is None: p.terminate()
 os.close(master)
