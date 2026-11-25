use super::{Fixture, TestResult, assert_code, write_json};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

fn digest(value: &Value) -> TestResultString {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
type TestResultString = Result<String, Box<dyn std::error::Error>>;
fn bound_catalog(f: &Fixture) -> TestResultString {
    let program = f.script(
        "brew",
        "printf invoked >> \"$MACOPS_TEST_HOME/native-calls\"; exit 7",
    )?;
    let prefix = f.home.join("native-prefix");
    let cellar = f.home.join("native-cellar");
    fs::create_dir(&prefix)?;
    fs::create_dir(&cellar)?;
    let helper = f.root.path().join("query.rb");
    let bytes = include_bytes!("../../src/brew/query-v1.rb");
    fs::write(&helper, bytes)?;
    let node = json!({"identity":{"device":7,"inode":8},"mode":33216,"size":32,"modified_ns":42});
    let context_id = format!("brew-context:{}", digest(&json!(["brew", prefix]))?);
    let id = format!("application:{}", digest(&json!(["brew", prefix, "foo"]))?);
    let query = json!({"kind":"brew-formula-native-v1","schema_version":1,"key":{"canonical_prefix":prefix,"full_name":"foo"},"selection":{"tap":"homebrew/core","name":"foo"},"context_epoch":0,
      "binding":{"context_id":context_id,"invocation":program,"canonical_program":program,"prefix":prefix,"cellar":cellar,"program_stamp":{"node":node,"sha256":"a".repeat(64)},"prefix_id":{"device":7,"inode":9},"cellar_id":{"device":7,"inode":10},"metadata_scope":{"home":f.home,"xdg_config_home":null,"homebrew_xdg_config_home":null,"actual_user_config_home":f.home.join(".homebrew"),"config_files":[{"state":"absent","path":"/etc/homebrew/brew.env","anchor":"/","parent":node},{"state":"absent","path":prefix.join("etc/homebrew/brew.env"),"anchor":prefix,"parent":node},{"state":"absent","path":f.home.join(".homebrew/brew.env"),"anchor":f.home,"parent":node}]},"compatibility":{"kind":"homebrew7-0-15","source_commit":"3e47189a707c739ee1278a28a3b845f5834574e6","acceptance_sha256":"b".repeat(64),"ruby_version":"4.0.9"}},
      "script":{"origin":"macops-builtin-v1","absolute_path":helper,"canonical_path":helper,"file_id":{"device":7,"inode":11},"sha256":format!("{:x}",Sha256::digest(bytes)),"size":bytes.len(),"product_build_id":"c".repeat(64)},"env_capture_id":"SYNTHETIC_PRIVATE_CAPTURE"});
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"] = json!([{"id":id,"name":"foo","kind":"cli","official_url":"https://brew.sh","provider":{"type":"brew-formula","query":query,"recipe_ref":null},"paths":[],"exact_version_required":false}]);
    write_json(&f.catalog, &catalog)?;
    Ok(id)
}
fn protected_command(f: &Fixture) -> std::process::Command {
    let original = f.command();
    if !cfg!(target_os = "macos") {
        return original;
    }
    let mut command = std::process::Command::new("/usr/bin/sandbox-exec");
    command.args(["-p",r#"(version 1)(allow default)(deny file-read-data (literal "/etc/homebrew/brew.env") (literal "/private/etc/homebrew/brew.env"))"#]).arg(original.get_program()).args(original.get_args());
    for (key, value) in original.get_envs() {
        match value {
            Some(value) => {
                command.env(key, value);
            }
            None => {
                command.env_remove(key);
            }
        }
    }
    command
}
fn absent_loader_slot(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            return Err("literal/fixture loader slot present; metadata-only test stopped".into());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    let mut parent = path.parent().ok_or("slot parent")?;
    loop {
        match fs::metadata(parent) {
            Ok(meta) if meta.is_dir() => break,
            Ok(_) => return Err("slot ancestor not directory".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                parent = parent.parent().ok_or("ancestor")?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    let anchor = parent.canonicalize()?;
    Ok(json!({"state":"absent","path":path,"anchor":anchor,"parent":real_node(&anchor)?}))
}
fn loader_slots(home: &Path, prefix: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    fs::create_dir_all(home.join(".homebrew"))?;
    Ok(json!([
        absent_loader_slot(Path::new("/etc/homebrew/brew.env"))?,
        absent_loader_slot(&prefix.join("etc/homebrew/brew.env"))?,
        absent_loader_slot(&home.join(".homebrew/brew.env"))?
    ]))
}
#[test]
fn bound_native_unknown_cannot_fall_back_to_legacy_empty_output_missing() -> TestResult {
    let f = Fixture::new()?;
    let id = bound_catalog(&f)?;
    let (_, data) = f.run(&["app", "doctor", &id])?;
    assert_eq!(data["data"][0]["id"], id, "{data}");
    assert_eq!(data["data"][0]["observation"]["status"], "unknown");
    assert!(!f.home.join("native-calls").exists());
    assert!(!f.root.path().join("state").exists());
    Ok(())
}
#[test]
fn unregistered_bound_preview_never_executes_native_or_creates_state() -> TestResult {
    let f = Fixture::new()?;
    let id = bound_catalog(&f)?;
    let (_output, data) = f.run(&["app", "upgrade", &id, "--dry-run"])?;
    let output = super::plain(&f, &["app", "upgrade", &id, "--dry-run"])?;
    assert_code(&output, 0);
    let text = String::from_utf8(output.stdout)?;
    assert!(text.contains("Homebrew 操作预演（未运行原生查询）"));
    assert!(
        text.contains("Formula foo") && text.contains("版本待核验 是"),
        "{text}"
    );
    assert!(text.contains("Cellar") && !text.contains("SYNTHETIC_PRIVATE_CAPTURE"));
    assert!(!f.home.join("native-calls").exists(), "{data}");
    assert!(!f.root.path().join("state").exists());
    assert!(!f.root.path().join("cache").exists());
    assert!(!data.to_string().contains("SYNTHETIC_PRIVATE_CAPTURE"));
    Ok(())
}

fn real_node(path: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::metadata(path)?;
    Ok(
        json!({"identity":{"device":m.dev(),"inode":m.ino()},"mode":m.mode(),"size":m.len(),"modified_ns":u64::try_from(i128::from(m.mtime())*1_000_000_000+i128::from(m.mtime_nsec()))?}),
    )
}
#[test]
fn registered_doctor_uses_frozen_private_query_and_never_creates_install_owner() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new()?;
    let id = bound_catalog(&f)?;
    let mut c = f.read_json("catalog.json")?;
    let q = &mut c["packages"][0]["provider"]["query"];
    let program = q["binding"]["invocation"]
        .as_str()
        .ok_or("program")?
        .to_owned();
    let prefix = q["binding"]["prefix"].as_str().ok_or("prefix")?.to_owned();
    let cellar = q["binding"]["cellar"].as_str().ok_or("cellar")?.to_owned();
    let response = f.home.join("native-response.json");
    fs::write(
        &program,
        format!(
            "#!/bin/sh\nprintf query >> '{}'\nprintf SYNTHETIC_PRIVATE_STDERR >&2\n/usr/bin/sed \"s/QUERY-FIXTURE/$6/g\" '{}'\n",
            f.home.join("native-calls").display(),
            response.display()
        ),
    )?;
    let n = real_node(Path::new(&program))?;
    q["binding"]["program_stamp"] =
        json!({"node":n,"sha256":format!("{:x}",Sha256::digest(fs::read(&program)?))});
    q["binding"]["prefix_id"] = real_node(Path::new(&prefix))?["identity"].clone();
    q["binding"]["cellar_id"] = real_node(Path::new(&cellar))?["identity"].clone();
    q["binding"]["metadata_scope"]["config_files"] = loader_slots(&f.home, Path::new(&prefix))?;
    let node = real_node(Path::new(&cellar))?;
    write_json(
        &response,
        &json!({"schema_version":1,"query_id":"QUERY-FIXTURE","homebrew_version":"7.0.15","selected_version":null,"prefix":prefix,"cellar":cellar,"actual_user_config_home":f.home.join(".homebrew"),"policies":{"no_auto_update":true,"no_install_cleanup":true,"no_autoremove":true,"runtime":{"no_bootsnap":true,"installed_dependents_check":true}},"rack":{"path":Path::new(&cellar).join("foo"),"cellar_before":node,"cellar_after":node,"rack_before":null,"rack_after":null,"entries":[],"kegs":[],"native_selected":[]},"formula":null}),
    )?;
    let context = q["binding"]["context_id"]
        .as_str()
        .ok_or("context")?
        .to_owned();
    let binding = q["binding"].clone();
    write_json(&f.catalog, &c)?;
    let state = f.root.path().join("state");
    fs::create_dir(&state)?;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
    write_json(
        &state.join("brew-contexts-v1.json"),
        &json!({"schema_version":1,"contexts":{context:{"epoch":0,"binding":binding,"observation_valid":false,"declaration_keys":{}}}}),
    )?;
    fs::set_permissions(
        state.join("brew-contexts-v1.json"),
        fs::Permissions::from_mode(0o600),
    )?;
    let actual = f.root.path().join("actual-home");
    fs::create_dir(&actual)?;
    let mut command = protected_command(&f);
    command.env_clear();
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let output = command
        .env("HOME", &actual)
        .env("PATH", "/usr/bin:/bin")
        .args(["app", "doctor", &id])
        .output()?;
    assert_code(&output, 3);
    let data: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        data["data"][0]["observation"]["status"],
        "brew-missing",
        "{data} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(f.home.join("native-calls"))?, "query");
    assert!(!state.join("receipts.json").exists());
    assert!(!data.to_string().contains("SYNTHETIC_PRIVATE_STDERR"));
    assert!(
        fs::read_dir(state.join("native-brew/query-scripts/v1"))?
            .next()
            .is_some()
    );
    assert_uninstall_noop(&f, &id, &actual, &state)?;
    Ok(())
}

fn assert_uninstall_noop(f: &Fixture, id: &str, actual: &Path, state: &Path) -> TestResult {
    let mut command = protected_command(f);
    command.env_clear();
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let output = command
        .env("HOME", actual)
        .env("PATH", "/usr/bin:/bin")
        .args(["app", "uninstall", id, "--yes"])
        .output()?;
    let result: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        result["data"]["phase"],
        "succeeded",
        "{result} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(f.home.join("native-calls"))?,
        "queryqueryquery"
    );
    assert!(!state.join("receipts.json").exists());
    let operation = result["data"]["id"].as_str().ok_or("operation")?;
    assert!(
        state
            .join("operations")
            .join(format!("{operation}.json"))
            .exists()
    );
    assert_eq!(result["data"]["plan"]["command"], Value::Null);
    Ok(())
}

const FAKE_NATIVE: &str = r"
import os,sys,json,pathlib,hashlib,stat,signal,time
h=pathlib.Path(__file__).parent
p=h/'native-prefix';c=h/'native-cellar';rack=c/'foo'
a=sys.argv[1:]
with (h/'events').open('a') as f:f.write(json.dumps(a)+'\n')
assert os.environ['HOME']==str(h)
assert os.environ['SYNTHETIC_AUTH']=='private-fixture-token'
assert os.environ['HOMEBREW_NO_AUTO_UPDATE']=='1'
assert os.environ['HOMEBREW_NO_INSTALL_CLEANUP']=='1'
assert os.environ['HOMEBREW_NO_AUTOREMOVE']=='1'
mode=(h/'mode').read_text().strip()
def node(p):
 s=p.lstat();return {'identity':{'device':s.st_dev,'inode':s.st_ino},'mode':s.st_mode,'size':s.st_size,'modified_ns':s.st_mtime_ns}
def stamp(p):return {'node':node(p),'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
if a[0]=='info':
 assert a==['info','--json=v2','--installed']
 if mode=='discovery-unavailable':sys.exit(11)
 print(json.dumps({'formulae':[{'name':'foo','full_name':'foo','installed':[{'version':'1.0.0'}]}],'casks':[{'token':'native-cask','full_token':'vendor/apps/native-cask','installed':'3.0.0'}]}));sys.exit(0)
if a[0]=='ruby':
 assert a[1]=='--' and pathlib.Path(a[2]).is_absolute() and a[3]=='observe' and a[4]=='homebrew/core/foo'
 if mode=='noop-recheck-failed' and len((h/'events').read_text().splitlines())==2:sys.exit(7)
 if mode=='bad-after' and (h/'mutated').exists():print('{}');sys.exit(0)
 ns=node(c);rs=node(rack) if rack.exists() else None
 rows=[];entries=[]
 if rack.exists():
  for k in sorted(rack.iterdir()):
   entries.append({'name':k.name,'kind':'directory','stamp':node(k)})
   rows.append({'path':str(k),'name':'foo','tap':'homebrew/core','full_name':'foo','spec':'stable','version':k.name,'receipt':stamp(k/'INSTALL_RECEIPT.json')})
 version=rows[-1]['version'] if rows else None
 info={'name':'foo','tap':'homebrew/core','full_name':'foo','active_spec':'stable','pkg_version':('1.2.3_1' if mode=='revision' else '2.0.0'),'pinned':False,'outdated':version!='2.0.0','resolution':{'alias_changed':False,'migration_needed':False},'latest_full_name':'foo','installed_paths':[r['path'] for r in rows]}
 if mode=='wrong-formula' or (mode=='wrong-formula-after' and (h/'mutated').exists()):info.update({'name':'other','tap':'vendor/tap','full_name':'vendor/tap/other','latest_full_name':'vendor/tap/other'})
 print(json.dumps({'schema_version':1,'query_id':a[5],'homebrew_version':'7.0.15','selected_version':version,'prefix':str(p),'cellar':str(c),'actual_user_config_home':str(h/'.homebrew'),'policies':{'no_auto_update':True,'no_install_cleanup':True,'no_autoremove':True,'runtime':{'no_bootsnap':True,'installed_dependents_check':True}},'rack':{'path':str(rack),'cellar_before':ns,'cellar_after':ns,'rack_before':rs,'rack_after':rs,'entries':entries,'kegs':rows,'native_selected':[r['path'] for r in rows]},'formula':info}))
 if mode=='before-drift' and len((h/'events').read_text().splitlines())==1:
  (rack/'1.0.0/INSTALL_RECEIPT.json').write_text('external-change')
 if mode=='owner-changed-at-commit' and len((h/'events').read_text().splitlines())==2:
  (h.parent/'catalog.json').write_text(json.dumps({'schema_version':1,'packages':[]}))
else:
 assert a in [['upgrade','--formula','homebrew/core/foo'],['install','--formula','homebrew/core/foo'],['uninstall','--formula','--force','homebrew/core/foo']],a
 if mode=='progress':
  print('fixture brew stdout progress',flush=True)
  print('fixture brew stderr progress',file=sys.stderr,flush=True)
  while not (h/'progress-release').exists():time.sleep(.01)
 (h/'mutated').write_text('once')
 if mode=='nonzero':sys.exit(23)
 if mode=='signal':os.kill(os.getpid(),signal.SIGTERM)
 if mode=='drift-env':(h/'.homebrew/brew.env').write_text('changed');sys.exit(0)
 if mode=='drift-root':c.rename(h/'old-cellar');c.mkdir();sys.exit(0)
 if a[0]=='uninstall':
  import shutil
  if rack.exists():shutil.rmtree(rack)
 else:
  k=rack/('3.0.0' if mode=='wrong-version' else '2.0.0');k.mkdir(parents=True,exist_ok=True);(k/'INSTALL_RECEIPT.json').write_text('{}')
";
fn lifecycle_fixture(
    f: &Fixture,
) -> Result<(String, std::path::PathBuf), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let id = bound_catalog(f)?;
    let fake = f.home.join("fake-native.py");
    fs::write(&fake, FAKE_NATIVE)?;
    let mut catalog = f.read_json("catalog.json")?;
    let q = &mut catalog["packages"][0]["provider"]["query"];
    let program = std::path::PathBuf::from(q["binding"]["invocation"].as_str().ok_or("program")?);
    fs::write(
        &program,
        format!(
            "#!/bin/sh\nexec /usr/bin/python3 '{}' \"$@\"\n",
            fake.display()
        ),
    )?;
    q["binding"]["program_stamp"] = json!({"node":real_node(&program)?,"sha256":format!("{:x}",Sha256::digest(fs::read(&program)?))});
    for (path, field) in [
        ("native-prefix", "prefix_id"),
        ("native-cellar", "cellar_id"),
    ] {
        q["binding"][field] = real_node(&f.home.join(path))?["identity"].clone();
    }
    q["binding"]["metadata_scope"]["config_files"] =
        loader_slots(&f.home, &f.home.join("native-prefix"))?;
    let context = q["binding"]["context_id"]
        .as_str()
        .ok_or("context")?
        .to_owned();
    let binding = q["binding"].clone();
    write_json(&f.catalog, &catalog)?;
    let state = f.root.path().join("state");
    fs::create_dir(&state)?;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
    write_json(
        &state.join("brew-contexts-v1.json"),
        &json!({"schema_version":1,"contexts":{context:{"epoch":0,"binding":binding,"observation_valid":false,"declaration_keys":{}}}}),
    )?;
    fs::set_permissions(
        state.join("brew-contexts-v1.json"),
        fs::Permissions::from_mode(0o600),
    )?;
    let keg = f.home.join("native-cellar/foo/1.0.0");
    fs::create_dir_all(&keg)?;
    fs::write(keg.join("INSTALL_RECEIPT.json"), b"{}")?;
    fs::write(f.home.join("mode"), b"ok")?;
    let actual = f.root.path().join("actual-home");
    fs::create_dir(&actual)?;
    Ok((id, actual))
}
fn lifecycle_run(
    f: &Fixture,
    actual: &Path,
    args: &[&str],
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut command = protected_command(f);
    command.env_clear();
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let output = command
        .env("HOME", actual)
        .env("PATH", "/usr/bin:/bin")
        .env("SYNTHETIC_AUTH", "private-fixture-token")
        .args(args)
        .output()?;
    let data: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "invalid CLI JSON for {args:?} ({}): {error}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    if data["data"]["nativeOutcome"]["reason"] == "native_completion_missing" {
        let state = f.root.path().join("state");
        for item in fs::read_dir(&state)? {
            let item = item?;
            if item
                .file_name()
                .to_string_lossy()
                .ends_with("before-error.json")
            {
                eprintln!("Brew diagnostic: {}", fs::read_to_string(item.path())?);
            }
        }
    }
    assert!(!data.to_string().contains("private-fixture-token"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-fixture-token"));
    Ok(data)
}
#[test]
fn bulk_upgrade_refreshes_the_bound_formula_and_runs_one_writer() -> TestResult {
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    let preview = lifecycle_run(&f, &actual, &["app", "upgrade", "--dry-run"])?;
    assert_eq!(preview["data"]["results"][0]["id"], id);
    assert_eq!(
        preview["data"]["results"][0]["status"],
        "native_query_required"
    );
    assert!(!f.home.join("events").exists());
    let upgraded = lifecycle_run(&f, &actual, &["app", "upgrade", "--yes"])?;
    assert_eq!(upgraded["status"], "ok", "{upgraded}");
    assert_eq!(
        upgraded["data"]["results"][0]["operation"]["journal"]["phase"],
        "succeeded"
    );
    assert!(f.home.join("native-cellar/foo/2.0.0").exists());
    let events: Vec<Value> = fs::read_to_string(f.home.join("events"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(events.iter().filter(|a| a[0] == "upgrade").count(), 1);
    assert!(events.iter().take_while(|a| a[0] != "upgrade").count() >= 2);
    Ok(())
}

#[test]
fn trusted_native_upgrade_and_full_uninstall_execute_once_without_adopt() -> TestResult {
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    let updated = lifecycle_run(&f, &actual, &["app", "refresh", &id])?;
    assert_eq!(
        updated["data"]["results"][0]["version"], "2.0.0",
        "{updated}"
    );
    assert!(!f.home.join("mutated").exists());
    let upgraded = lifecycle_run(&f, &actual, &["app", "upgrade", &id, "--yes"])?;
    assert_eq!(
        upgraded["data"]["journal"]["phase"], "succeeded",
        "{upgraded}"
    );
    assert!(f.home.join("native-cellar/foo/2.0.0").exists());
    let removed = lifecycle_run(&f, &actual, &["app", "uninstall", &id, "--yes"])?;
    assert_eq!(
        removed["data"]["journal"]["phase"], "succeeded",
        "{removed}"
    );
    assert!(!f.home.join("native-cellar/foo").exists());
    let events: Vec<Value> = fs::read_to_string(f.home.join("events"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(events.iter().filter(|a| a[0] == "upgrade").count(), 1);
    assert_eq!(events.iter().filter(|a| a[0] == "uninstall").count(), 1);
    assert_eq!(events.iter().filter(|a| a[0] == "ruby").count(), 7);
    Ok(())
}
#[test]
fn adopted_brew_formula_remains_bound_to_its_original_context_without_a_catalog_entry() -> TestResult
{
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    let adopted = lifecycle_run(&f, &actual, &["app", "adopt", &id, "--yes"])?;
    assert_eq!(adopted["data"]["phase"], "succeeded", "{adopted}");
    let common = f.read_json("state/receipts.json")?;
    assert!(common["installations"].get(&id).is_some());
    write_json(&f.catalog, &json!({"schema_version":1,"packages":[]}))?;
    let original_events = fs::read(f.home.join("events"))?;
    let preview = lifecycle_run(&f, &actual, &["app", "uninstall", &id, "--dry-run"])?;
    assert_eq!(preview["data"]["nativeRuns"], 0);
    assert_eq!(preview["data"]["scope"]["full_name"], "foo");
    assert_eq!(fs::read(f.home.join("events"))?, original_events);
    assert_eq!(f.read_json("state/receipts.json")?, common);

    let removed = lifecycle_run(&f, &actual, &["app", "uninstall", &id, "--yes"])?;
    assert_eq!(
        removed["data"]["journal"]["phase"], "succeeded",
        "{removed}"
    );
    assert!(!f.home.join("native-cellar/foo").exists());
    assert!(
        f.read_json("state/receipts.json")?["installations"]
            .get(&id)
            .is_none()
    );
    let events = fs::read_to_string(f.home.join("events"))?;
    assert_eq!(
        events
            .lines()
            .filter(|line| line.starts_with("[\"uninstall\""))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn recover_settles_the_authoritative_active_brew_record_across_a_terminal_history_gap() -> TestResult
{
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    let first = lifecycle_run(&f, &actual, &["app", "upgrade", &id, "--yes"])?;
    assert_eq!(first["data"]["journal"]["phase"], "succeeded", "{first}");
    let operation = first["data"]["journal"]["id"].as_str().ok_or("operation")?;
    let active_path = f.file("state/active.json");
    let mut active = f.read_json("state/active.json")?;
    active["phase"] = json!("unknown");
    active["message"] = json!("fixture before the terminal active record was published");
    write_json(&active_path, &active)?;
    let history_path = f.file("state/operations").join(format!("{operation}.json"));
    let history = fs::read(&history_path)?;
    let receipts_path = f.file("state/receipts.json");
    let receipts = fs::read(&receipts_path)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&history)?["phase"],
        "succeeded"
    );
    let pending = fs::read(&active_path)?;
    let preview = lifecycle_run(&f, &actual, &["recover", operation, "--dry-run"])?;
    assert_eq!(preview["data"]["persistentWrites"], false);
    assert_eq!(preview["data"]["journal"]["phase"], "unknown");
    assert_eq!(fs::read(&active_path)?, pending);

    let recovered = lifecycle_run(&f, &actual, &["recover", operation])?;
    assert_eq!(
        recovered["data"]["journal"]["phase"], "succeeded",
        "{recovered}"
    );
    assert_eq!(f.read_json("state/active.json")?["phase"], "succeeded");
    assert_eq!(f.read_json("state/active.json")?["id"], operation);
    assert_eq!(fs::read(&history_path)?, history);
    assert_eq!(fs::read(&receipts_path)?, receipts);
    fs::remove_file(&history_path)?;
    let from_active = lifecycle_run(&f, &actual, &["recover", operation])?;
    assert_eq!(from_active["data"]["journal"]["phase"], "succeeded");
    assert_eq!(from_active["data"]["persistentWrites"], false);
    assert!(!history_path.exists());
    assert_eq!(
        fs::read_to_string(f.home.join("events"))?
            .lines()
            .filter(|line| line.starts_with(r#"["upgrade""#))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn original_nonzero_and_bad_after_remain_unknown_without_native_retry() -> TestResult {
    for mode in [
        "nonzero",
        "bad-after",
        "signal",
        "drift-env",
        "drift-root",
        "wrong-version",
    ] {
        let f = Fixture::new()?;
        let (id, actual) = lifecycle_fixture(&f)?;
        fs::write(f.home.join("mode"), mode)?;
        let result = lifecycle_run(&f, &actual, &["app", "upgrade", &id, "--yes"])?;
        assert_eq!(
            result["data"]["journal"]["phase"], "unknown",
            "{mode}: {result}"
        );
        if mode == "nonzero" {
            assert_eq!(result["data"]["nativeOutcome"]["code"], 23);
        }
        if mode == "signal" {
            assert_eq!(result["data"]["nativeOutcome"]["signal"], 15);
        }
        assert!(!f.root.path().join("state/receipts.json").exists());
        let op = result["data"]["journal"]["id"]
            .as_str()
            .ok_or("operation")?;
        let recovered = lifecycle_run(&f, &actual, &["recover", op])?;
        assert_eq!(
            recovered["data"]["journal"]["phase"], "unknown",
            "{recovered}"
        );
        let resolved = lifecycle_run(&f, &actual, &["recover", op, "--resolve-failed", "--yes"])?;
        assert_eq!(resolved["data"]["journal"]["phase"], "failed", "{resolved}");
        let events = fs::read_to_string(f.home.join("events"))?;
        assert_eq!(
            events
                .lines()
                .filter(|line| line.contains("\"upgrade\""))
                .count(),
            1,
            "{events}"
        );
    }
    Ok(())
}

#[test]
fn native_update_preserves_revision_and_install_uses_native_missing_scope() -> TestResult {
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    fs::write(f.home.join("mode"), "revision")?;
    let updated = lifecycle_run(&f, &actual, &["app", "refresh", &id])?;
    assert_eq!(
        updated["data"]["results"][0]["version"], "1.2.3_1",
        "{updated}"
    );
    assert!(!f.home.join("mutated").exists());
    fs::write(f.home.join("mode"), "ok")?;
    fs::remove_dir_all(f.home.join("native-cellar/foo"))?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"]["recipe_ref"] = json!("synthetic-declared-formula");
    write_json(&f.catalog, &catalog)?;
    let installed = lifecycle_run(&f, &actual, &["app", "install", &id, "--yes"])?;
    assert_eq!(
        installed["data"]["journal"]["phase"], "succeeded",
        "{installed}"
    );
    let state = f.root.path().join("state");
    let mut captures = 0;
    for item in fs::read_dir(&state)? {
        let item = item?;
        if item.path().is_file()
            && fs::read(item.path())?
                .windows(b"private-fixture-token".len())
                .any(|w| w == b"private-fixture-token")
        {
            captures += 1;
        }
    }
    assert_eq!(
        captures, 1,
        "env must have exactly one private persisted capture"
    );
    Ok(())
}

#[test]
fn another_registry_does_not_trust_a_new_context_and_first_explicit_action_confirms() -> TestResult
{
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    let registry = f.root.path().join("state/brew-contexts-v1.json");
    write_json(&registry, &json!({"schema_version":1,"contexts":{}}))?;
    let doctor = lifecycle_run(&f, &actual, &["app", "doctor", &id])?;
    assert_eq!(
        doctor["error"]["code"], "brew_context_unregistered",
        "{doctor}"
    );
    assert!(!f.home.join("events").exists());
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&registry)?)?["contexts"],
        json!({})
    );
    let unconfirmed = lifecycle_run(&f, &actual, &["app", "upgrade", &id])?;
    assert_eq!(
        unconfirmed["error"]["code"], "confirmation_required",
        "{unconfirmed}"
    );
    assert!(!f.home.join("events").exists());
    let confirmed = lifecycle_run(&f, &actual, &["app", "upgrade", &id, "--yes"])?;
    assert_eq!(
        confirmed["data"]["journal"]["phase"], "succeeded",
        "{confirmed}"
    );
    Ok(())
}

#[test]
fn fresh_before_query_blocks_keg_drift_without_launch_or_recovery_retry() -> TestResult {
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    fs::write(f.home.join("mode"), "before-drift")?;
    let result = lifecycle_run(&f, &actual, &["app", "upgrade", &id, "--yes"])?;
    assert_eq!(result["data"]["journal"]["phase"], "unknown", "{result}");
    assert!(!f.home.join("mutated").exists());
    assert_eq!(
        fs::read_to_string(f.home.join("events"))?.lines().count(),
        2
    );
    let op = result["data"]["journal"]["id"]
        .as_str()
        .ok_or("operation")?;
    let error: Value = serde_json::from_slice(&fs::read(
        f.root
            .path()
            .join("state")
            .join(format!("brew-native-{op}-before-error.json")),
    )?)?;
    assert_eq!(error["reason"], "brew_before_changed");
    let recovered = lifecycle_run(&f, &actual, &["recover", op])?;
    assert_eq!(
        recovered["data"]["journal"]["phase"], "unknown",
        "{recovered}"
    );
    assert_eq!(
        fs::read_to_string(f.home.join("events"))?.lines().count(),
        2
    );
    Ok(())
}

#[test]
fn unregistered_discovery_and_application_inventory_do_not_execute_path_brew() -> TestResult {
    let f = Fixture::new()?;
    f.script("brew", "printf query >> \"$MACOPS_TEST_HOME/discovery-calls\"; case \"$1\" in --prefix) printf '%s' \"$MACOPS_TEST_HOME/native-prefix\";; *) printf '%s' '{\"formulae\":[],\"casks\":[]}' ;; esac")?;
    let _ = f.run(&["app", "discover", "brew"])?;
    assert!(!f.home.join("discovery-calls").exists());
    let _ = f.run(&["app", "resources"])?;
    assert!(!f.home.join("discovery-calls").exists());
    Ok(())
}

#[test]
fn first_interactive_query_consent_does_not_authorize_the_actual_writer() -> TestResult {
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    write_json(
        &f.root.path().join("state/brew-contexts-v1.json"),
        &json!({"schema_version":1,"contexts":{}}),
    )?;
    let command = protected_command(&f);
    let mut argv: Vec<String> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|s| s.to_str().ok_or("argv").map(str::to_owned))
        .collect::<Result<_, _>>()?;
    argv.extend(["app".into(), "upgrade".into(), id]);
    let mut env =
        json!({"HOME":actual,"PATH":"/usr/bin:/bin","SYNTHETIC_AUTH":"private-fixture-token"});
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        env["LLVM_PROFILE_FILE"] = json!(profile.to_str().ok_or("profile")?);
    }
    let request = f.home.join("pty-request.json");
    write_json(
        &request,
        &json!({"argv":argv,"env":env,"events":f.home.join("events")}),
    )?;
    let script = r"
import os,sys,json,pty,subprocess,selectors,time,pathlib
q=json.load(open(sys.argv[1]));master,slave=pty.openpty()
p=subprocess.Popen(q['argv'],stdin=slave,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=q['env'],start_new_session=True)
os.close(slave);sel=selectors.DefaultSelector();sel.register(p.stdout,selectors.EVENT_READ,'out');sel.register(p.stderr,selectors.EVENT_READ,'err')
buf={'out':b'','err':b''};handled=0;counts=[];deadline=time.monotonic()+30
while sel.get_map():
 if time.monotonic()>deadline:p.terminate();raise RuntimeError('fixture interaction timeout')
 for key,_ in sel.select(.1):
  data=os.read(key.fileobj.fileno(),65536)
  if not data:sel.unregister(key.fileobj);continue
  buf[key.data]+=data
  prompts=buf['err'].decode('utf-8').count('同意就输入 yes')
  while handled<prompts:
   path=pathlib.Path(q['events']);counts.append(len(path.read_text().splitlines()) if path.exists() else 0)
   os.write(master,b'yes\n' if handled==0 else b'no\n');handled+=1
code=p.wait();os.close(master)
print(json.dumps({'stdout':buf['out'].decode(),'stderr':buf['err'].decode(),'prompts':handled,'native_at_prompts':counts,'exit':code}))
";
    let output = std::process::Command::new("/usr/bin/python3")
        .args(["-I", "-S", "-c", script])
        .arg(request)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["prompts"], 2, "{result}");
    assert_eq!(result["native_at_prompts"], json!([0, 1]), "{result}");
    assert!(!f.home.join("mutated").exists());
    assert_eq!(result["exit"], 3);
    Ok(())
}

#[test]
fn wrong_formula_cannot_publish_an_index_or_settle_upgrade() -> TestResult {
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    fs::write(f.home.join("mode"), "wrong-formula")?;
    let update = lifecycle_run(&f, &actual, &["app", "refresh", &id])?;
    assert_eq!(
        update["error"]["code"], "brew_formula_identity_mismatch",
        "{update}"
    );
    assert!(!f.root.path().join("state/version-indexes.json").exists());
    assert!(!f.home.join("mutated").exists());
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    fs::write(f.home.join("mode"), "wrong-formula-after")?;
    let upgraded = lifecycle_run(&f, &actual, &["app", "upgrade", &id, "--yes"])?;
    assert_eq!(
        upgraded["data"]["journal"]["phase"], "unknown",
        "{upgraded}"
    );
    assert!(!f.root.path().join("state/receipts.json").exists());
    assert!(
        f.home.join("mutated").exists(),
        "postcondition test requires one actual Native mutation"
    );
    assert_eq!(upgraded["data"]["nativeOutcome"]["code"], 0, "{upgraded}");
    Ok(())
}
#[test]
fn already_current_native_upgrade_settles_noop_with_zero_writer() -> TestResult {
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    let keg = f.home.join("native-cellar/foo/2.0.0");
    fs::create_dir(&keg)?;
    fs::write(keg.join("INSTALL_RECEIPT.json"), b"{}")?;
    let result = lifecycle_run(&f, &actual, &["app", "upgrade", &id, "--yes"])?;
    assert_eq!(result["data"]["phase"], "succeeded", "{result}");
    assert_eq!(result["data"]["plan"]["brew"]["execution"], "verified-noop");
    assert!(result["data"]["plan"]["command"].is_null());
    assert!(!f.home.join("mutated").exists());
    let events: Vec<Value> = fs::read_to_string(f.home.join("events"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|a| a[0] == "ruby"));
    Ok(())
}
#[test]
fn brew_noop_cannot_settle_after_a_failed_recheck_or_a_changed_owner() -> TestResult {
    for mode in ["noop-recheck-failed", "owner-changed-at-commit"] {
        let f = Fixture::new()?;
        let (id, actual) = lifecycle_fixture(&f)?;
        let keg = f.home.join("native-cellar/foo/2.0.0");
        fs::create_dir(&keg)?;
        let receipt = keg.join("INSTALL_RECEIPT.json");
        fs::write(&receipt, b"{}")?;
        fs::write(f.home.join("mode"), mode)?;
        let result = lifecycle_run(&f, &actual, &["app", "upgrade", &id])?;
        assert_eq!(result["data"]["phase"], "unknown", "{mode}: {result}");
        assert_eq!(result["data"]["plan"]["brew"]["execution"], "verified-noop");
        assert_eq!(f.read_json("state/active.json")?["phase"], "unknown");
        assert_eq!(fs::read(receipt)?, b"{}");
        assert!(!f.file("state/receipts.json").exists());
        assert!(!f.home.join("mutated").exists());
        let events: Vec<Value> = fs::read_to_string(f.home.join("events"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| event[0] == "ruby"));
        if mode == "owner-changed-at-commit" {
            assert!(
                result["data"]["message"]
                    .as_str()
                    .ok_or("message")?
                    .contains("brew_owner_changed")
            );
            assert_eq!(f.read_json("catalog.json")?["packages"], json!([]));
        }
    }
    Ok(())
}

#[test]
fn verified_bound_discovery_reads_absolute_program_and_preserves_casks_without_adoption()
-> TestResult {
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    let pending = lifecycle_run(&f, &actual, &["app", "discover", "brew"])?;
    assert_eq!(pending["data"]["coverage"]["state"], "unavailable");
    assert_eq!(pending["data"]["packages"], json!([]));
    assert_eq!(pending["data"]["unresolved"], true);
    assert!(!f.home.join("events").exists());
    let observed = lifecycle_run(&f, &actual, &["app", "doctor", &id])?;
    assert_eq!(
        observed["data"][0]["observation"]["status"], "brew-installed",
        "{observed}"
    );
    let result = lifecycle_run(&f, &actual, &["app", "discover", "brew"])?;
    assert_eq!(result["data"]["adopted"], false);
    let packages = result["data"]["packages"].as_array().ok_or("packages")?;
    assert!(
        packages
            .iter()
            .any(|p| p["manager"] == "brew" && p["native_id"] == "foo"),
        "{result}"
    );
    assert!(
        packages
            .iter()
            .any(|p| p["manager"] == "brew-cask" && p["native_id"] == "vendor/apps/native-cask"),
        "{result}"
    );
    assert!(!f.root.path().join("state/receipts.json").exists());
    let events: Vec<Value> = fs::read_to_string(f.home.join("events"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1][0], "info");

    let other_prefix = f.home.join("unverified-prefix");
    let other_cellar = other_prefix.join("Cellar");
    fs::create_dir_all(&other_cellar)?;
    let other_context = format!("brew-context:{}", digest(&json!(["brew", other_prefix]))?);
    let mut registry = f.read_json("state/brew-contexts-v1.json")?;
    let contexts = registry["contexts"].as_object_mut().ok_or("contexts")?;
    let mut unverified = contexts.values().next().ok_or("verified binding")?.clone();
    unverified["observation_valid"] = json!(false);
    unverified["binding"]["context_id"] = json!(other_context);
    unverified["binding"]["prefix"] = json!(other_prefix);
    unverified["binding"]["cellar"] = json!(other_cellar);
    unverified["binding"]["prefix_id"] = real_node(&other_prefix)?["identity"].clone();
    unverified["binding"]["cellar_id"] = real_node(&other_cellar)?["identity"].clone();
    unverified["binding"]["metadata_scope"]["config_files"] = loader_slots(&f.home, &other_prefix)?;
    contexts.insert(other_context, unverified);
    write_json(&f.file("state/brew-contexts-v1.json"), &registry)?;
    let mixed = lifecycle_run(&f, &actual, &["app", "discover", "brew"])?;
    assert_eq!(mixed["data"]["coverage"]["state"], "unavailable");
    assert_eq!(mixed["data"]["packages"], result["data"]["packages"]);
    assert_eq!(mixed["data"]["unresolved"], true);
    assert_eq!(
        fs::read_to_string(f.home.join("events"))?.lines().count(),
        3
    );
    Ok(())
}

#[test]
fn bound_discovery_preserves_the_registry_when_native_info_fails_or_loader_metadata_changes()
-> TestResult {
    for verification in ["discovery-unavailable", "loader-drift"] {
        let f = Fixture::new()?;
        let (id, actual) = lifecycle_fixture(&f)?;
        let observed = lifecycle_run(&f, &actual, &["app", "doctor", &id])?;
        assert_eq!(
            observed["data"][0]["observation"]["status"],
            "brew-installed"
        );
        let registry_path = f.file("state/brew-contexts-v1.json");
        let registry = fs::read(&registry_path)?;
        if verification == "loader-drift" {
            fs::write(
                f.home.join(".homebrew/brew.env"),
                b"changed synthetic configuration",
            )?;
        } else {
            fs::write(f.home.join("mode"), verification)?;
        }
        let result = lifecycle_run(&f, &actual, &["app", "discover", "brew"])?;
        assert_eq!(result["data"]["coverage"]["state"], "unavailable");
        assert_eq!(result["data"]["packages"], json!([]));
        assert_eq!(result["data"]["unresolved"], true);
        assert_eq!(fs::read(registry_path)?, registry);
        assert!(!f.file("state/receipts.json").exists());
        assert!(!f.home.join("mutated").exists());
        let events = fs::read_to_string(f.home.join("events"))?;
        if verification == "loader-drift" {
            assert_eq!(events.lines().count(), 1);
        } else {
            assert_eq!(events.lines().count(), 2);
            assert_eq!(
                result["data"]["coverage"]["issues"],
                json!(["brew_query_failed"])
            );
        }
    }
    Ok(())
}

pub(super) fn register_discovery_fixture(f: &Fixture, prefix: &Path) -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let cellar = prefix.join("Cellar");
    fs::create_dir_all(&cellar)?;
    let program = f.home.join("bin/brew");
    let context = format!("brew-context:{}", digest(&json!(["brew", prefix]))?);
    let binding = json!({"context_id":context,"invocation":program,"canonical_program":program.canonicalize()?,"prefix":prefix.canonicalize()?,"cellar":cellar.canonicalize()?,"program_stamp":{"node":real_node(&program)?,"sha256":format!("{:x}",Sha256::digest(fs::read(&program)?))},"prefix_id":real_node(prefix)?["identity"],"cellar_id":real_node(&cellar)?["identity"],"metadata_scope":{"home":f.home,"xdg_config_home":null,"homebrew_xdg_config_home":null,"actual_user_config_home":f.home.join(".homebrew"),"config_files":loader_slots(&f.home,prefix)?},"compatibility":{"kind":"homebrew7-0-15","source_commit":"3e47189a707c739ee1278a28a3b845f5834574e6","acceptance_sha256":"b".repeat(64),"ruby_version":"4.0.9"}});
    let state = f.root.path().join("state");
    fs::create_dir_all(&state)?;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
    let registry = state.join("brew-contexts-v1.json");
    write_json(
        &registry,
        &json!({"schema_version":1,"contexts":{context:{"epoch":0,"binding":binding,"observation_valid":true,"declaration_keys":{}}}}),
    )?;
    fs::set_permissions(registry, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn writer_scope_confirmed(f: &Fixture) -> Result<bool, Box<dyn std::error::Error>> {
    f.read_json("state/brew-contexts-v1.json")?["contexts"]
        .as_object()
        .ok_or("contexts")?
        .values()
        .next()
        .ok_or("context")?["writer_scope_confirmed"]
        .as_bool()
        .ok_or_else(|| "missing writer permission fact".into())
}
#[test]
fn noop_and_baseline_adoption_do_not_confirm_native_writer_scope() -> TestResult {
    for baseline in ["noop", "adopt"] {
        let f = Fixture::new()?;
        let (id, actual) = lifecycle_fixture(&f)?;
        if baseline == "noop" {
            let keg = f.home.join("native-cellar/foo/2.0.0");
            fs::create_dir(&keg)?;
            fs::write(keg.join("INSTALL_RECEIPT.json"), b"{}")?;
            let result = lifecycle_run(&f, &actual, &["app", "upgrade", &id])?;
            assert_eq!(result["data"]["phase"], "succeeded", "{result}");
            fs::remove_dir_all(keg)?;
        } else {
            let result = lifecycle_run(&f, &actual, &["app", "adopt", &id, "--yes"])?;
            assert_eq!(result["data"]["phase"], "succeeded", "{result}");
        }
        assert!(!writer_scope_confirmed(&f)?);
        let next = lifecycle_run(&f, &actual, &["app", "upgrade", &id])?;
        assert_eq!(next["error"]["code"], "confirmation_required", "{next}");
        assert!(!f.home.join("mutated").exists());
        assert!(!writer_scope_confirmed(&f)?);
    }
    Ok(())
}
#[test]
fn confirmed_native_writer_scope_allows_daily_upgrade_without_reconfirmation() -> TestResult {
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    let first = lifecycle_run(&f, &actual, &["app", "upgrade", &id, "--yes"])?;
    assert_eq!(first["data"]["journal"]["phase"], "succeeded", "{first}");
    assert!(writer_scope_confirmed(&f)?);
    fs::remove_dir_all(f.home.join("native-cellar/foo/2.0.0"))?;
    let next = lifecycle_run(&f, &actual, &["app", "upgrade", &id])?;
    assert_eq!(next["data"]["journal"]["phase"], "succeeded", "{next}");
    assert!(writer_scope_confirmed(&f)?);
    let events: Vec<Value> = fs::read_to_string(f.home.join("events"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(events.iter().filter(|a| a[0] == "upgrade").count(), 2);
    Ok(())
}

#[test]
fn independent_refusal_keeps_first_writer_confirmation_pending() -> TestResult {
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    write_json(
        &f.root.path().join("state/brew-contexts-v1.json"),
        &json!({"schema_version":1,"contexts":{}}),
    )?;
    let command = protected_command(&f);
    let mut argv: Vec<String> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|s| s.to_str().ok_or("argv").map(str::to_owned))
        .collect::<Result<_, _>>()?;
    argv.extend(["app".to_owned(), "upgrade".to_owned(), id.clone()]);
    let mut env =
        json!({"HOME":actual,"PATH":"/usr/bin:/bin","SYNTHETIC_AUTH":"private-fixture-token"});
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        env["LLVM_PROFILE_FILE"] = json!(profile.to_str().ok_or("profile")?);
    }
    let request = f.home.join("pty-request.json");
    write_json(
        &request,
        &json!({"argv":argv,"env":env,"events":f.home.join("events")}),
    )?;
    let script = r"
import os,sys,json,pty,subprocess,selectors,time,pathlib
q=json.load(open(sys.argv[1]));master,slave=pty.openpty()
p=subprocess.Popen(q['argv'],stdin=slave,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=q['env'],start_new_session=True)
os.close(slave);sel=selectors.DefaultSelector();sel.register(p.stdout,selectors.EVENT_READ,'out');sel.register(p.stderr,selectors.EVENT_READ,'err')
buf={'out':b'','err':b''};handled=0;counts=[];deadline=time.monotonic()+30
while sel.get_map():
 if time.monotonic()>deadline:p.terminate();raise RuntimeError('fixture interaction timeout')
 for key,_ in sel.select(.1):
  data=os.read(key.fileobj.fileno(),65536)
  if not data:sel.unregister(key.fileobj);continue
  buf[key.data]+=data
  prompts=buf['err'].decode('utf-8').count('同意就输入 yes')
  while handled<prompts:
   path=pathlib.Path(q['events']);counts.append(len(path.read_text().splitlines()) if path.exists() else 0)
   os.write(master,b'yes\n' if handled==0 else b'no\n');handled+=1
code=p.wait();os.close(master)
print(json.dumps({'stdout':buf['out'].decode(),'stderr':buf['err'].decode(),'prompts':handled,'native_at_prompts':counts,'exit':code}))
";
    let output = std::process::Command::new("/usr/bin/python3")
        .args(["-I", "-S", "-c", script])
        .arg(request)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(result["prompts"], 2, "{result}");
    assert_eq!(result["native_at_prompts"], json!([0, 1]), "{result}");
    assert!(!f.home.join("mutated").exists());
    assert_eq!(result["exit"], 3);
    let registry = f.read_json("state/brew-contexts-v1.json")?;
    let observation_valid = registry["contexts"]
        .as_object()
        .ok_or("contexts")?
        .values()
        .next()
        .ok_or("context")?["observation_valid"]
        .clone();
    let next = lifecycle_run(&f, &actual, &["app", "upgrade", &id])?;
    let writes = f.home.join("mutated").exists();
    eprintln!(
        "review: refusal_exit={} native_at_prompts={} observation_valid={} second_status={} second_phase={} second_consent={} fake_wrote={writes}",
        result["exit"],
        result["native_at_prompts"],
        observation_valid,
        next["status"],
        next["data"]["journal"]["phase"],
        next["data"]["journal"]["plan"]["brew"]["confirmation"]["consent"]
    );
    assert!(
        !writes,
        "a declined write needs a fresh actual-scope confirmation"
    );
    Ok(())
}

#[test]
fn independent_read_queries_do_not_authorize_a_first_writer() -> TestResult {
    let mut all_blocked = true;
    for prefix in [vec!["app", "doctor"], vec!["app", "refresh", "--dry-run"]] {
        let f = Fixture::new()?;
        let (id, actual) = lifecycle_fixture(&f)?;
        let before = f.read_json("state/brew-contexts-v1.json")?;
        assert_eq!(
            before["contexts"]
                .as_object()
                .ok_or("contexts")?
                .values()
                .next()
                .ok_or("context")?["observation_valid"],
            false
        );
        let mut args = prefix.clone();
        args.push(id.as_str());
        let result = lifecycle_run(&f, &actual, &args)?;
        assert_eq!(result["status"], "ok");
        assert!(!f.home.join("mutated").exists());
        let registry = f.read_json("state/brew-contexts-v1.json")?;
        let observation_valid = registry["contexts"]
            .as_object()
            .ok_or("contexts")?
            .values()
            .next()
            .ok_or("context")?["observation_valid"]
            .clone();
        let next = lifecycle_run(&f, &actual, &["app", "upgrade", &id])?;
        let writes = f.home.join("mutated").exists();
        eprintln!(
            "review: query={prefix:?} observation_valid={} first_writer_status={} first_writer_phase={} first_writer_consent={} fake_wrote={writes}",
            observation_valid,
            next["status"],
            next["data"]["journal"]["phase"],
            next["data"]["journal"]["plan"]["brew"]["confirmation"]["consent"]
        );
        all_blocked &= !writes;
    }
    assert!(
        all_blocked,
        "read-only scope confirmation cannot authorize a first writer"
    );
    Ok(())
}

#[test]
fn adopt_brew_by_the_name_shown_in_apps() -> TestResult {
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["name"] = json!("Formula Display Label");
    write_json(&f.catalog, &catalog)?;
    let refreshed = lifecycle_run(&f, &actual, &["app", "refresh", &id])?;
    assert_eq!(refreshed["status"], "ok", "{refreshed}");
    let events = fs::read(f.home.join("events"))?;
    let preview = lifecycle_run(&f, &actual, &["app", "adopt", "foo", "--dry-run"])?;
    assert_eq!(preview["data"]["id"], id, "{preview}");
    assert_eq!(preview["data"]["nativeRuns"], 0, "{preview}");
    assert_eq!(fs::read(f.home.join("events"))?, events);
    assert!(!f.file("state/receipts.json").exists());
    let listed = lifecycle_run(&f, &actual, &["app", "resources"])?;
    assert!(
        listed["data"]["resources"]
            .as_array()
            .ok_or("apps")?
            .iter()
            .any(|item| item["name"] == "foo" && item["id"] == id && item["manager"] == "brew")
    );
    let adopted = lifecycle_run(&f, &actual, &["app", "adopt", "foo", "--yes"])?;
    assert_eq!(adopted["data"]["phase"], "succeeded", "{adopted}");
    let receipts = f.read_json("state/receipts.json")?;
    assert_eq!(receipts["installations"][&id]["package"]["id"], id);
    assert!(!f.home.join("mutated").exists());
    assert!(!writer_scope_confirmed(&f)?);
    Ok(())
}

#[test]
fn adopt_brew_name_collision_refuses_but_explicit_id_still_works() -> TestResult {
    let f = Fixture::new()?;
    let (id, actual) = lifecycle_fixture(&f)?;
    let program = f.script("same-name", "exit 92")?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"]
        .as_array_mut()
        .ok_or("catalog")?
        .push(json!({
            "id": "other-foo", "name": "foo", "kind": "cli",
            "official_url": "https://example.test/foo",
            "provider": {"type":"command","query":{"program":program,"args":[]}},
            "paths": [], "exact_version_required": false
        }));
    write_json(&f.catalog, &catalog)?;
    for args in [
        ["app", "adopt", "foo", "--yes"],
        ["app", "adopt", "foo", "--dry-run"],
    ] {
        let ambiguous = lifecycle_run(&f, &actual, &args)?;
        assert_eq!(ambiguous["error"]["code"], "ambiguous", "{ambiguous}");
        assert!(
            ambiguous["error"]["message"]
                .as_str()
                .ok_or("message")?
                .contains("mo app")
        );
        assert!(!f.file("state/receipts.json").exists());
        assert!(!f.home.join("events").exists());
    }
    let adopted = lifecycle_run(&f, &actual, &["app", "adopt", &id, "--yes"])?;
    assert_eq!(adopted["data"]["phase"], "succeeded", "{adopted}");
    assert!(
        f.read_json("state/receipts.json")?["installations"]
            .get(&id)
            .is_some()
    );
    assert!(!f.home.join("mutated").exists());
    Ok(())
}

#[test]
fn inventory_confirms_adoption_upgrade_and_removal_with_native_brew() -> TestResult {
    for (key, result, mutation, approved, receipt_only) in [
        ("a", "纳入管理结果 已完成", None, true, false),
        ("u", "已取消", None, false, false),
        ("u", "升级结果 已完成", Some("upgrade"), true, false),
        ("x", "卸载结果 已完成", Some("uninstall"), true, false),
        ("x", "卸载结果 已完成", Some("uninstall"), true, true),
    ] {
        let f = Fixture::new()?;
        let (id, actual) = lifecycle_fixture(&f)?;
        let refreshed = lifecycle_run(&f, &actual, &["app", "refresh", &id])?;
        assert_eq!(refreshed["status"], "ok", "{refreshed}");
        if receipt_only {
            let adopted = lifecycle_run(&f, &actual, &["app", "adopt", &id, "--yes"])?;
            assert_eq!(adopted["status"], "ok", "{adopted}");
            write_json(&f.catalog, &json!({"schema_version":1,"packages":[]}))?;
        }
        let listed = lifecycle_run(&f, &actual, &["app", "resources"])?;
        let formulae: Vec<_> = listed["data"]["resources"]
            .as_array()
            .ok_or("software inventory")?
            .iter()
            .filter(|row| row["name"] == "foo")
            .collect();
        assert_eq!(formulae.len(), 1, "{listed}");
        assert_eq!(formulae[0]["id"], id);
        assert_eq!(formulae[0]["manager"], "brew");
        assert_eq!(formulae[0]["recipe_ref"], id);
        let old = format!(
            "application:{}",
            digest(&json!(["declared-recipe", id, f.home]))?
        );
        assert!(
            formulae[0]["aliases"]
                .as_array()
                .ok_or("aliases")?
                .contains(&json!(old))
        );
        let registry = f.file("state/brew-contexts-v1.json");
        let mut book = f.read_json("state/brew-contexts-v1.json")?;
        book["contexts"]
            .as_object_mut()
            .ok_or("contexts")?
            .values_mut()
            .next()
            .ok_or("context")?["writer_scope_confirmed"] = json!(true);
        write_json(&registry, &book)?;
        run_brew_pty(
            &f,
            &actual,
            &["app"],
            json!({"inventory":[key,result],"approved":approved}),
        )?;
        let commands: Vec<Value> = fs::read_to_string(f.home.join("events"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        let mutations: Vec<_> = commands
            .iter()
            .filter_map(|event| event[0].as_str())
            .filter(|command| matches!(*command, "install" | "upgrade" | "uninstall"))
            .collect();
        assert_eq!(mutations, mutation.into_iter().collect::<Vec<_>>());
        assert_eq!(f.home.join("mutated").exists(), mutation.is_some());
        assert_eq!(f.home.join("native-cellar/foo").exists(), key != "x");
        if approved && (key != "x" || receipt_only) {
            let receipts = f.read_json("state/receipts.json")?;
            assert_eq!(receipts["installations"].get(&id).is_some(), key != "x");
        } else {
            assert!(!f.file("state/receipts.json").exists());
        }
    }
    Ok(())
}

fn run_brew_pty(f: &Fixture, actual: &Path, invocation: &[&str], mut request: Value) -> TestResult {
    let command = protected_command(f);
    let argv: Vec<_> = std::iter::once(command.get_program())
        .chain(command.get_args().filter(|arg| *arg != "--json"))
        .map(|arg| arg.to_str().ok_or("argument"))
        .chain(invocation.iter().copied().map(Ok))
        .collect::<Result<_, _>>()?;
    let mut env = json!({"HOME":actual,"PATH":"/usr/bin:/bin","TERM":"xterm-256color",
        "SYNTHETIC_AUTH":"private-fixture-token"});
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        env["LLVM_PROFILE_FILE"] = json!(profile.to_str().ok_or("profile")?);
    }
    request["argv"] = json!(argv);
    request["env"] = env;
    request["catalog"] = json!(f.catalog);
    request["events"] = json!(f.home.join("events"));
    request["mutated"] = json!(f.home.join("mutated"));
    let path = f.home.join("brew-pty.json");
    write_json(&path, &request)?;
    let result = std::process::Command::new("/usr/bin/python3")
        .args(["-I", "-S", "-c", include_str!("recommend_brew_pty.py")])
        .arg(path)
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}

#[test]
fn declining_first_brew_use_stops_the_other_targets_in_an_application_batch() -> TestResult {
    let f = Fixture::new()?;
    let indexed = f.read_json("catalog.json")?["packages"][0].clone();
    let (_, actual) = lifecycle_fixture(&f)?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(indexed);
    write_json(&f.catalog, &catalog)?;
    let registry = f.file("state/brew-contexts-v1.json");
    write_json(&registry, &json!({"schema_version":1,"contexts":{}}))?;
    fs::write(f.file("version"), "1.0.0")?;
    run_brew_pty(
        &f,
        &actual,
        &["app", "upgrade"],
        json!({"mode":"batch-discovery-cancel"}),
    )?;
    assert_eq!(fs::read_to_string(f.file("version"))?, "1.0.0");
    assert!(!f.file("calls").exists());
    assert!(!f.home.join("mutated").exists());
    assert_eq!(
        f.read_json("state/brew-contexts-v1.json")?["contexts"],
        json!({})
    );
    Ok(())
}

#[test]
fn brew_recovery_reuses_the_frozen_recipe_and_requires_consent_for_a_changed_source() -> TestResult
{
    let f = Fixture::new()?;
    let mut shadow = f.read_json("catalog.json")?["packages"][0].clone();
    let (id, actual) = lifecycle_fixture(&f)?;
    shadow["id"] = json!("app:foo");
    let mut original_catalog = f.read_json("catalog.json")?;
    original_catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(shadow);
    write_json(&f.catalog, &original_catalog)?;
    let result = lifecycle_run(&f, &actual, &["app", "upgrade", &id, "--yes"])?;
    let mut pending = f.read_json("state/active.json")?;
    assert_eq!(pending["phase"], "succeeded", "{result}");
    pending["phase"] = json!("unknown");
    let active = f.file("state/active.json");
    write_json(&active, &pending)?;
    let before = fs::read(&active)?;
    let events = fs::read(f.home.join("events"))?;

    let mut changed = original_catalog.clone();
    changed["packages"][0]["provider"]["recipe_ref"] = json!("a-different-recipe");
    write_json(&f.catalog, &changed)?;
    let blocked = lifecycle_run(&f, &actual, &["app", "upgrade", "app:foo"])?;
    assert_eq!(
        blocked["error"]["code"], "confirmation_required",
        "{blocked}"
    );
    assert_eq!(fs::read(&active)?, before);
    assert_eq!(fs::read(f.home.join("events"))?, events);

    write_json(&f.catalog, &original_catalog)?;
    let retry = lifecycle_run(&f, &actual, &["app", "upgrade", "app:foo"])?;
    assert_eq!(retry["data"]["journal"]["id"], pending["id"], "{retry}");
    assert_eq!(retry["data"]["journal"]["phase"], "succeeded");
    let events = fs::read_to_string(f.home.join("events"))?;
    assert_eq!(
        events
            .lines()
            .filter(|line| line.starts_with(r#"["upgrade""#))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn recommendation_cannot_advance_an_unrelated_journal_when_recovery_consent_is_declined()
-> TestResult {
    let f = Fixture::new()?;
    let generic = f.read_json("catalog.json")?["packages"][0].clone();
    let (_, actual) = lifecycle_fixture(&f)?;
    fs::remove_dir_all(f.home.join("native-cellar/foo"))?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"]["recipe_ref"] = json!("synthetic-declared-formula");
    catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(generic);
    write_json(&f.catalog, &catalog)?;
    let (out, completed) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    let mut old = completed["data"].clone();
    old["phase"] = json!("unknown");
    let active = f.file("state/active.json");
    write_json(&active, &old)?;
    let before = fs::read(&active)?;
    let history = f.file(&format!(
        "state/operations/{}.json",
        old["id"].as_str().ok_or("ID")?
    ));
    let history_before = fs::read(&history)?;
    let batch = lifecycle_run(&f, &actual, &["app", "upgrade"])?;
    assert_eq!(batch["error"]["code"], "confirmation_required", "{batch}");
    assert_eq!(fs::read(&active)?, before);
    assert_eq!(fs::read(&history)?, history_before);
    assert!(!f.home.join("events").exists());

    run_brew_pty(
        &f,
        &actual,
        &["recommend", "foo"],
        json!({"mode":"pending-cancel"}),
    )?;
    assert_eq!(fs::read(active)?, before);
    assert_eq!(fs::read(history)?, history_before);
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    assert!(!f.home.join("events").exists());
    Ok(())
}

#[test]
fn recommend_brew_confirms_selected_native_install_even_in_a_trusted_context() -> TestResult {
    for mode in ["cancel", "confirm", "stale", "trust-stale"] {
        let approved = mode == "confirm";
        let f = Fixture::new()?;
        let (id, actual) = lifecycle_fixture(&f)?;
        if approved {
            fs::write(f.home.join("mode"), "progress")?;
        }
        fs::remove_dir_all(f.home.join("native-cellar/foo"))?;
        let mut catalog = f.read_json("catalog.json")?;
        catalog["packages"][0]["provider"]["recipe_ref"] = json!("synthetic-declared-formula");
        write_json(&f.catalog, &catalog)?;
        let registry = f.file("state/brew-contexts-v1.json");
        let mut book = f.read_json("state/brew-contexts-v1.json")?;
        let context = book["contexts"]
            .as_object_mut()
            .ok_or("contexts")?
            .values_mut()
            .next()
            .ok_or("context")?;
        context["writer_scope_confirmed"] = json!(true);
        if mode == "trust-stale" {
            book["contexts"] = json!({});
        }
        write_json(&registry, &book)?;
        run_brew_pty(
            &f,
            &actual,
            &["recommend", "foo"],
            json!({"approved":approved,"mode":mode}),
        )?;
        if matches!(mode, "stale" | "trust-stale") {
            assert!(!f.home.join("events").exists());
            assert!(!f.file("state/receipts.json").exists());
            continue;
        }
        let commands: Vec<Value> = fs::read_to_string(f.home.join("events"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert_eq!(
            commands
                .iter()
                .filter(|event| event[0] == "install")
                .count(),
            usize::from(approved)
        );
        assert_eq!(f.home.join("mutated").exists(), approved);
        if approved {
            let receipts = f.read_json("state/receipts.json")?;
            assert!(receipts["installations"].get(&id).is_some());
        } else {
            assert!(!f.file("state/receipts.json").exists());
        }
    }
    Ok(())
}
