"""Compile bounded UV policy/identity/CAS mutants and require behavioral failures."""
import os,pathlib,shutil,subprocess,tempfile
BASE=pathlib.Path(__file__).resolve().parent.parent
ART=BASE/'artifacts/uv-readonly/policy-mutants';ART.mkdir(parents=True,exist_ok=True)

def content(file):return (BASE/'crates/app/src'/file).read_text()
P=content('policy/uv_local.rs');T=content('catalog/target.rs');O=content('software/uv_ownership.rs');L=content('software/uv_local.rs');R=content('operations/recovery.rs')
V=content('operations/providers.rs');begin=V.index('    if crate::policy::uv_local::descriptor',V.index('pub fn capture_query'));end=V.index('    let query = match',begin)
mutants=[
 ('namespace-recapture','operations/providers.rs',V[:begin]+V[end:],'operations::uv_tests::uv_adopted_namespace_replacement_cannot_be_silently_recaptured'),
 ('uv-action','policy/uv_local.rs',P.replace('if is_uv(provider) && action != Action::Adopt','if false && is_uv(provider) && action != Action::Adopt'),'policy::uv_local::tests::only_bound_adopt_is_permitted_and_cannot_lend_authority'),
 ('owner-early-return','catalog/target.rs',T.replace('''    if crate::policy::uv_local::is_uv(&package.provider) {
        return crate::software::uv_ownership::declared(ctx, package);
    }
''',''),'operations::uv_tests::uv_resolve_and_final_publication_recheck_declaration_owner_history'),
 ('owner-snapshot-cas','software/uv_ownership.rs',O.replace('if expected != current','if false && expected != current'),'operations::uv_tests::uv_resolve_and_final_publication_recheck_declaration_owner_history'),
 ('wrong-home-id','policy/uv_local.rs',P.replace('&("uv", root, normalize_name(name)?)','&("uv", Path::new("/HOME"), normalize_name(name)?)'),'policy::uv_local::tests::aliases_and_separate_roots'),
 ('external-receipt-path','software/uv_local.rs',L.replace('''install_path.parent() != Some(bin.path.as_path())
            || ''',''),'software::uv_local::tests::budgets_duplicates_receipt_escape_and_lock_competition'),
 ('root-stamp','software/uv_local.rs',L.replace('root.identity != query.namespace.tool_dir.identity','false'),'software::uv_local::tests::stable_tool_and_each_data_leaf_fail_closed'),
 ('old-recover-no-query','operations/recovery.rs',R.replace('''    if matches!(
        journal.plan.package.provider,
        Provider::Uv { binding: None, .. }
    ) && !resolve_failed''','''    if false && matches!(
        journal.plan.package.provider,
        Provider::Uv { binding: None, .. }
    ) && !resolve_failed'''),'operations::uv_tests::uv_prepared_receipt_cas_and_old_uv_recover_remain_no_spawn'),
]
for name,file,source,test in mutants:
 assert source!=content(file),name
 with tempfile.TemporaryDirectory(prefix='uv-policy-'+name+'-',dir='/private/tmp') as temporary:
  root=pathlib.Path(temporary);shutil.copytree(BASE,root,dirs_exist_ok=True,ignore=shutil.ignore_patterns('.git','target','artifacts','__pycache__'))
  (root/'crates/app/src'/file).write_text(source);(ART/(name+'.rs')).write_text(source)
  env=os.environ.copy();env.update(RUSTC_WRAPPER='',CARGO_TARGET_DIR='/private/tmp/macops-build-uv-policy-mutants')
  result=subprocess.run(['cargo','test','--locked','--offline','--lib',test,'--','--exact'],cwd=root,env=env,capture_output=True,text=True,timeout=180)
  output=result.stdout+result.stderr;(ART/(name+'.log')).write_text(output)
  assert 'Finished `test`' in output and 'running 1 test' in output and result.returncode!=0 and 'FAILED' in output,(name,output)
  print(name,'VALID KILLED',flush=True)
