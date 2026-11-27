# Synthetic Native package protocol, never a source-skill script.
import json, os, pathlib, shutil, sys, time
home=pathlib.Path(os.environ['HOME']); args=sys.argv[1:]
if args==['--version']: print('1.9.0');sys.exit(0)
verb=args[0]
if verb=='check':
    with (home/'native-checks.jsonl').open('a') as file:file.write(json.dumps(args)+'\n')
    sys.exit(0)
name=args[args.index('--rename')+1] if '--rename' in args else args[args.index('--skill')+1]
with (home/'native-calls.jsonl').open('a') as file:file.write(json.dumps({'pid':os.getpid(),'args':args,'name':name})+'\n')
mode=(home/'native-mode').read_text().strip() if (home/'native-mode').exists() else 'normal'
lock=home/'.agents/.skill-lock.json'; target=home/'.agents/skills'/name
try: book=json.loads(lock.read_text())
except FileNotFoundError: book={'version':3,'skills':{}}
if verb=='remove':
    shutil.rmtree(target);book['skills'].pop(name);lock.write_text(json.dumps(book));print('fixture Skills CLI removed '+name, flush=True);sys.exit(0)
if mode=='wait':
    while not (home/'continue-native').exists():time.sleep(.03)
source=args[1].split('github.com/')[1].split('/');commit=source[3]
if target.exists():shutil.rmtree(target)
target.mkdir(parents=True)
(target/'SKILL.md').write_text('---\nname: '+name+'\ndescription: fixture\n---\nbody\n')
if mode=='bad-yaml':
    (target/'agents').mkdir();(target/'agents/openai.yaml').write_text('policy: []\n')
book['skills'][name]={'source':'/'.join(source[:2]),'sourceType':'github','sourceUrl':'https://github.com/'+'/'.join(source[:2])+'.git','commit':commit,'ref':commit,'skillName':args[args.index('--skill')+1],'skillPath':'/'.join(source[4:])+'/SKILL.md','skillFolderHash':'b'*40,'installMode':'copy','installedAt':'2026-11-20T00:00:00.000Z','updatedAt':'2026-11-20T00:00:00.000Z'}
if mode=='corrupt-row':book['skills']['other']['source']='foreign/change'
if mode=='corrupt-client':(home/'.claude-mi/skills/unselected/SKILL.md').write_text('foreign overwrite')
lock.write_text(json.dumps(book))
if mode=='partial':sys.exit(1)
