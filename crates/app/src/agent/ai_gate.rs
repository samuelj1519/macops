use crate::{
    agent::ai_payload::Evidence,
    agent::ai_policy as policy,
    context::Context,
    domain::{CommandSpec, Scope},
    error::{Error, Result},
    platform::fs::{self as paths, Directory},
    platform::process,
};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub fn executable(ctx: &Context, name: &str) -> Result<PathBuf> {
    let path = crate::operations::command::executable(name)?;
    if ctx.isolated && !paths::read_path(&path)?.starts_with(&ctx.home) {
        return Err(Error::attention(
            "manager_missing",
            "合成环境不调用真实工具",
        ));
    }
    Ok(path)
}
pub fn query(scope: &Scope, program: &Path, args: Vec<String>) -> Result<String> {
    let output = crate::operations::command::query(
        scope,
        &CommandSpec {
            program: program.into(),
            args,
        },
    )?;
    if output.code != Some(0) || output.uncertain {
        return Err(Error::attention(
            "ai_probe_failed",
            "AI 命令失败、超时或输出超过限制",
        ));
    }
    Ok(output.stdout.trim().into())
}
pub fn version(ctx: &Context, program: &Path, tool: &str) -> Result<String> {
    observed_version(ctx, program, tool).map(|(_, version)| version)
}

pub(crate) fn observed_version(
    ctx: &Context,
    program: &Path,
    tool: &str,
) -> Result<(PathBuf, String)> {
    let mut evidence = Evidence::default();
    let output = probe_captured(
        ctx,
        None,
        program,
        vec![if tool == "mimo" { "-v" } else { "--version" }.into()],
        &mut evidence,
    )?;
    evidence.verify()?;
    Ok((
        evidence.resolved_file(program)?.into(),
        policy::running_version(&output)?,
    ))
}

pub fn managed_version(ctx: &Context, mise: &Path, tool: &str, target: &str) -> Result<String> {
    let spec = format!(
        "{}@{target}",
        crate::config::rules::config_tx::ai_mise_spec(tool)?
    );
    let (root, binary) = installed(ctx, mise, &spec, policy::binary(tool)?)?;
    let mut evidence = Evidence::default();
    evidence.tree(&root)?;
    let observed = policy::running_version(&probe_captured(
        ctx,
        Some(mise),
        &binary,
        vec!["--version".into()],
        &mut evidence,
    )?)?;
    evidence.verify()?;
    Ok(observed)
}

fn installed_binary(ctx: &Context, mise: &Path, spec: &str, binary: &str) -> Result<PathBuf> {
    installed(ctx, mise, spec, binary).map(|(_, binary)| binary)
}
fn installed(ctx: &Context, mise: &Path, spec: &str, binary: &str) -> Result<(PathBuf, PathBuf)> {
    let installed = paths::read_path(&PathBuf::from(query(
        &ctx.scope,
        mise,
        vec!["where".into(), spec.into()],
    )?))?;
    if ctx.isolated && !installed.starts_with(&ctx.home) {
        return Err(Error::attention(
            "ai_payload_outside_fixture",
            "版本探测目标不在合成环境内",
        ));
    }
    let directory = Directory::open(&installed)?;
    let program = paths::read_path(&PathBuf::from(query(
        &ctx.scope,
        mise,
        vec!["which".into(), "--tool".into(), spec.into(), binary.into()],
    )?))?;
    if !program.starts_with(&installed) {
        return Err(Error::attention(
            "ai_binary_outside_install",
            "解析后的二进制不属于指定安装版本",
        ));
    }
    regular(&program, true)?;
    directory.verify()?;
    Ok((installed, program))
}

fn regular(path: &Path, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let resolved = paths::read_path(path)?;
    let meta = std::fs::metadata(&resolved)?;
    if !meta.is_file() || (executable && meta.permissions().mode() & 0o111 == 0) {
        return Err(Error::attention(
            "ai_payload_missing",
            "缺少完整的可执行载荷",
        ));
    }
    Ok(())
}
fn package_root(root: &Path, name: &str) -> Result<PathBuf> {
    let candidates = [
        root.join("lib/node_modules").join(name),
        root.join("node_modules").join(name),
    ];
    for candidate in candidates {
        if candidate.is_dir() {
            let resolved = paths::read_path(&candidate)?;
            if resolved.starts_with(paths::read_path(root)?) {
                return Ok(resolved);
            }
        }
    }
    Err(Error::attention(
        "ai_payload_missing",
        "安装目录缺少目标 npm 包",
    ))
}
fn dependency(package: &Path, install: &Path, name: &str) -> Result<PathBuf> {
    for path in [
        package.join("node_modules").join(name),
        install.join("lib/node_modules").join(name),
        install.join("node_modules").join(name),
    ] {
        if path.is_dir() && paths::read_path(&path)?.starts_with(paths::read_path(install)?) {
            return Ok(path);
        }
    }
    Err(Error::attention("ai_payload_missing", "缺少必须的原生依赖"))
}

pub fn payload(ctx: &Context, mise: &Path, tool: &str, target: &str) -> Result<Evidence> {
    let spec = format!(
        "{}@{target}",
        crate::config::rules::config_tx::ai_mise_spec(tool)?
    );
    let (root, binary) = installed(ctx, mise, &spec, policy::binary(tool)?)?;
    let mut evidence = Evidence::default();
    evidence.tree(&root)?;
    if tool == "dsh" {
        dsh(ctx, mise, &root, &mut evidence)?;
    } else if tool == "codegraph" {
        codegraph(&root, &mut evidence)?;
    }
    let observed = policy::running_version(&probe_captured(
        ctx,
        Some(mise),
        &binary,
        vec!["--version".into()],
        &mut evidence,
    )?)?;
    if observed != target {
        return Err(Error::attention(
            "ai_version_mismatch",
            "实际运行版本与目标版本不同",
        ));
    }
    evidence.verify()?;
    Ok(evidence)
}

fn dsh(ctx: &Context, mise: &Path, root: &Path, evidence: &mut Evidence) -> Result<()> {
    let package = package_root(root, "@deepseek-ai/dsh")?;
    let sharp = dependency(&package, root, "sharp")?;
    payload_file(root, &sharp.join("package.json"), false, evidence)?;
    let native = dependency(&package, root, "@img/sharp-darwin-arm64")?;
    payload_file(
        root,
        &native.join("lib/sharp-darwin-arm64.node"),
        false,
        evidence,
    )?;
    let libvips = dependency(&package, root, "@img/sharp-libvips-darwin-arm64")?;
    let lib = Directory::open(&libvips.join("lib"))?;
    let mut found = false;
    for item in lib.names()? {
        let item = item.map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
        let Ok(name) = item.file_name().to_str() else {
            continue;
        };
        if name.starts_with("libvips-cpp.")
            && Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("dylib"))
        {
            payload_file(root, &lib.path.join(name), false, evidence)?;
            found = true;
        }
    }
    lib.verify()?;
    if !found {
        return Err(Error::attention(
            "ai_payload_missing",
            "缺少 libvips-cpp 动态库",
        ));
    }
    let module = serde_json::to_string(&sharp)?;
    let node = installed_binary(ctx, mise, "node", "node")?;
    probe_captured(
        ctx,
        Some(mise),
        &node,
        vec!["-e".into(), format!("require({module})")],
        evidence,
    )?;
    Ok(())
}
fn codegraph(root: &Path, evidence: &mut Evidence) -> Result<()> {
    let package = package_root(root, "@colbymchenry/codegraph")?;
    payload_file(root, &package.join("npm-shim.js"), false, evidence)?;
    let bundle = dependency(&package, root, "@colbymchenry/codegraph-darwin-arm64")?;
    payload_file(root, &bundle.join("node"), true, evidence)?;
    payload_file(root, &bundle.join("bin/codegraph"), true, evidence)?;
    let directory = Directory::open(&bundle)?;
    directory.metadata(OsStr::new("node"))?;
    directory.verify()
}

fn payload_file(root: &Path, path: &Path, executable: bool, evidence: &mut Evidence) -> Result<()> {
    if !paths::read_path(path)?.starts_with(paths::read_path(root)?) {
        return Err(Error::attention(
            "ai_payload_outside_install",
            "必需载荷不属于目标安装目录",
        ));
    }
    regular(path, executable)?;
    evidence.file(path)
}

fn interpreter(ctx: &Context, mise: Option<&Path>, header: &[u8]) -> Result<Option<PathBuf>> {
    if !header.starts_with(b"#!") {
        return Ok(None);
    }
    let first = header
        .split(|b| *b == b'\n')
        .next()
        .ok_or_else(|| Error::new("invalid_ai_binary", "解释器入口为空"))?;
    let text = std::str::from_utf8(&first[2..])
        .map_err(|_| Error::attention("ai_interpreter_unsupported", "解释器不是有效 UTF-8"))?;
    let words: Vec<_> = text.split_whitespace().collect();
    let path = match words.as_slice() {
        ["/usr/bin/env", name @ ("node" | "bun")] => runtime(ctx, mise, name)?,
        [name]
            if Path::new(name)
                .file_name()
                .is_some_and(|n| n == "node" || n == "bun") =>
        {
            let name = Path::new(name)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::new("invalid_ai_binary", "解释器文件名无效"))?;
            runtime(ctx, mise, name)?
        }
        ["/usr/bin/env", "bash"] => PathBuf::from("/bin/bash"),
        ["/usr/bin/env", "sh"] => PathBuf::from("/bin/sh"),
        [name @ ("/bin/sh" | "/bin/bash" | "/bin/zsh" | "/usr/bin/python3")] => PathBuf::from(name),
        _ => {
            return Err(Error::attention(
                "ai_interpreter_unsupported",
                "不能冻结此脚本解释器，拒绝执行",
            ));
        }
    };
    Ok(Some(path))
}
fn probe_captured(
    ctx: &Context,
    mise: Option<&Path>,
    binary: &Path,
    mut args: Vec<String>,
    evidence: &mut Evidence,
) -> Result<String> {
    use std::os::unix::process::CommandExt;
    let mut program = binary.to_path_buf();
    let mut directories = vec![
        binary
            .parent()
            .ok_or_else(|| Error::new("invalid_ai_binary", "二进制缺少父目录"))?
            .to_path_buf(),
    ];
    let mut resolved = false;
    for _ in 0..4 {
        let header = evidence.header(&program)?;
        let Some(next) = interpreter(ctx, mise, &header)? else {
            resolved = true;
            break;
        };
        evidence.file(&next)?;
        directories.insert(
            0,
            next.parent()
                .ok_or_else(|| Error::new("invalid_ai_binary", "解释器缺少目录"))?
                .to_path_buf(),
        );
        if next
            .file_name()
            .is_some_and(|n| n == "sh" || n == "bash" || n == "zsh")
            && let Ok(node) = runtime(ctx, mise, "node")
        {
            evidence.file(&node)?;
            directories.insert(
                0,
                node.parent()
                    .ok_or_else(|| Error::new("invalid_ai_binary", "Node 缺少目录"))?
                    .to_path_buf(),
            );
        }
        args.insert(
            0,
            program
                .to_str()
                .ok_or_else(|| Error::new("invalid_ai_binary", "入口路径不是 UTF-8"))?
                .into(),
        );
        program = next;
    }
    if !resolved {
        return Err(Error::attention(
            "ai_interpreter_unsupported",
            "解释器链超出有界读取上限",
        ));
    }
    directories.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    let path = std::env::join_paths(directories)
        .map_err(|_| Error::new("invalid_ai_binary", "无法冻结探测 PATH"))?;
    let mut command =
        crate::operations::command::command(&CommandSpec { program, args }, &ctx.scope)?;
    command.env("PATH", path).process_group(0);
    let output = process::capture(&mut command, std::time::Duration::from_secs(60))?;
    if output.code != Some(0) || output.uncertain {
        return Err(Error::attention(
            "ai_probe_failed",
            "冻结解释器探测失败或超时",
        ));
    }
    Ok(output.stdout.trim().into())
}

fn runtime(ctx: &Context, mise: Option<&Path>, name: &str) -> Result<PathBuf> {
    let manager = mise
        .map(Path::to_path_buf)
        .map_or_else(|| executable(ctx, "mise"), Ok)?;
    installed_binary(ctx, &manager, name, name)
}
