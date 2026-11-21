use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "macops",
    version,
    about = "本机软件、Agent 资产与目录的统一维护入口",
    long_about = "查询来源、安装实例与目录；写命令默认执行，使用 --dry-run 预演。未知来源和恢复状态不会自动跳过。"
)]
pub struct Cli {
    #[arg(long,global=true,default_value_t=120,value_parser=clap::value_parser!(u64).range(1..=3600),help="单次写入操作超时秒数")]
    pub timeout_secs: u64,
    #[arg(long, global = true, help = "输出结构化 JSON")]
    pub json: bool,
    #[arg(long, global = true, help = "预演，不执行安装器或持久业务写入")]
    pub dry_run: bool,
    #[arg(long, global = true, help = "跳过交互确认，仍执行所有校验")]
    pub yes: bool,
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,
    #[arg(long, global = true)]
    pub settings: Option<PathBuf>,
    #[arg(long, global = true, help = "macops 自有配置目录")]
    pub config_dir: Option<PathBuf>,
    #[arg(long, global = true, help = "macops 自有持久数据目录")]
    pub data_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    pub state_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    pub cache_dir: Option<PathBuf>,
    #[arg(long, global = true, help = "显式 catalog JSON；缺失或损坏时失败")]
    pub catalog: Option<PathBuf>,
    #[arg(long, global = true, help = "明确选择 chezmoi 原生 source owner")]
    pub chezmoi_source: Option<PathBuf>,
    #[arg(long, global = true, help = "明确选择 chezmoi 原生 destination")]
    pub chezmoi_destination: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "使用指定原生配置，保留其 data、hooks 与模板语义"
    )]
    pub chezmoi_config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "一次性转换旧治理数据；保留旧源并校验分叉")]
    Migrate {
        #[command(subcommand)]
        command: Migrate,
    },
    #[command(alias = "ls", about = "列出登记的软件与来源，不运行安装器")]
    List {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        project: Option<PathBuf>,
    },
    #[command(about = "导出本机管理清单或预演已有清单")]
    Manifest {
        #[command(subcommand)]
        command: Manifest,
    },
    #[command(about = "查看一个唯一安装对象")]
    Info {
        id: String,
        #[arg(long)]
        project: Option<PathBuf>,
    },
    #[command(about = "查询已声明来源或分析提供的安装证据")]
    Recommend {
        product: String,
        #[arg(long)]
        evidence: Option<PathBuf>,
        #[arg(
            long,
            conflicts_with = "evidence",
            help = "读取已明确选定的公开 GitHub 仓库"
        )]
        github: Option<String>,
        #[arg(long, help = "使用 DeepSeek 解释随程序提供的公开证据")]
        model: bool,
        #[arg(long, requires = "model")]
        keychain_account: Option<String>,
    },
    #[command(about = "通过原生管理器发现安装；不自动接管")]
    Discover { manager: Option<String> },
    #[command(about = "检查本地安装，损坏输入报告 unknown")]
    Doctor { id: Option<String> },
    #[command(about = "采纳当前安装基线，不重新安装")]
    Adopt {
        id: String,
        /// 将已采纳的 npm 实例绑定到显式声明的原生配方
        #[arg(long)]
        bind_recipe: bool,
    },
    #[command(about = "按官方渠道安装一个对象")]
    Install {
        id: String,
        #[arg(long)]
        version: Option<String>,
    },
    #[command(about = "升级一个对象；不会隐式整机升级")]
    Upgrade {
        id: String,
        #[arg(long)]
        version: Option<String>,
    },
    #[command(about = "卸载明确对象，保留配置与用户数据")]
    Uninstall { id: String },
    #[command(about = "刷新可用版本索引，不升级软件")]
    Update { id: Option<String> },
    #[command(about = "根据已有索引显示可更新或未知项")]
    Outdated { id: Option<String> },
    #[command(about = "查询准确目录与用途")]
    Dirs {
        id: Option<String>,
        #[arg(
            long,
            conflicts_with = "id",
            help = "查看 macops 自有目录与实际配置路径"
        )]
        roots: bool,
    },
    #[command(about = "清理 macops 自有缓存；原生共享缓存只提供建议")]
    Cleanup { id: String },
    #[command(about = "查看操作历史")]
    History { id: Option<String> },
    #[command(about = "查询原操作结果，不重跑安装器")]
    Recover {
        operation_id: String,
        #[arg(long, help = "人工核对后记录失败并解除阻断；不修复软件")]
        resolve_failed: bool,
    },
    #[command(about = "技能与插件来源维护")]
    Skills {
        #[command(subcommand)]
        command: Option<Skills>,
    },
    #[command(about = "AI CLI 与固定版本维护")]
    Ai {
        #[command(subcommand)]
        command: Ai,
    },
    #[command(about = "配置归属与差异")]
    Config {
        #[command(subcommand)]
        command: Option<Config>,
    },
    #[command(about = "应用查询")]
    App {
        #[command(subcommand)]
        command: App,
    },
    #[command(about = "浏览已发现的软件与安装来源")]
    Apps,
    #[command(about = "浏览 Agent 和 Harness 档案")]
    Agents,
    #[command(about = "工具链查询")]
    Toolchain {
        #[command(subcommand)]
        command: Toolchain,
    },
    #[command(about = "已声明服务")]
    Service {
        #[command(subcommand)]
        command: Service,
    },
    #[command(about = "凭据元信息；永不读取密钥值")]
    Credential {
        #[command(subcommand)]
        command: Credential,
    },
    #[command(name = "__npm_observe", hide = true)]
    NpmObserve {
        #[arg(long)]
        npm: PathBuf,
        #[arg(long)]
        prefix: PathBuf,
        #[arg(long)]
        name: String,
    },
    #[command(name = "__worker", hide = true)]
    Worker { journal: PathBuf },
    #[command(name = "__native_worker", hide = true)]
    NativeWorker { journal: PathBuf },
}

#[derive(Debug, Subcommand)]
pub enum Migrate {
    #[command(about = "只读检查转换、原位保留、归档与阻断项")]
    Preflight,
    #[command(about = "执行已通过预检的一次性转换")]
    Run,
    #[command(about = "重复核验新权威与旧源分叉；不覆盖数据")]
    Verify,
    #[command(about = "核验并收口原迁移；不重复执行外部命令")]
    Recover { migration_id: String },
}

#[derive(Debug, Args)]
pub struct SkillSelection {
    pub name: Option<String>,
    #[arg(long)]
    pub revision: Option<String>,
    #[arg(long = "only")]
    pub only: Vec<String>,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub change_id: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum Skills {
    #[command(about = "查询客户端原生插件的固定安装配方")]
    Plugins {
        #[command(subcommand)]
        command: NativePlugins,
    },
    Info {
        id: String,
    },
    Install {
        #[arg(long)]
        recipe: PathBuf,
    },
    Adopt {
        id: String,
        #[arg(long)]
        recipe: PathBuf,
    },
    Remove {
        id: String,
    },
    #[command(alias = "ls")]
    List {
        name: Option<String>,
        #[arg(long)]
        details: bool,
        #[arg(long)]
        only: Vec<String>,
    },
    Check {
        name: Option<String>,
        #[arg(long)]
        only: Vec<String>,
    },
    Status {
        name: Option<String>,
        #[arg(long)]
        only: Vec<String>,
    },
    Update(SkillSelection),
    Outdated {
        name: Option<String>,
        #[arg(long)]
        only: Vec<String>,
    },
    Upgrade(SkillSelection),
    Sync(SkillSelection),
    Policy {
        #[command(subcommand)]
        command: Policy,
    },
}

#[derive(Debug, Subcommand)]
pub enum NativePlugins {
    #[command(about = "查看绑定和本地插件证据")]
    List {
        #[arg(long)]
        instance: Option<String>,
    },
    #[command(about = "检查一个固定配方的本地插件证据")]
    Check { recipe: String },
    #[command(about = "按固定配方升级已启用的同来源插件")]
    Upgrade { recipe: String },
}

#[derive(Debug, Subcommand)]
pub enum Policy {
    Check,
    Sync {
        #[arg(long,value_parser=crate::policy::skills::parse_change_id)]
        change_id: Option<String>,
    },
    Recover {
        #[arg(long,value_parser=crate::policy::skills::parse_change_id)]
        change_id: Option<String>,
    },
}
#[derive(Debug, Subcommand)]
pub enum Ai {
    #[command(alias = "ls")]
    List,
    Upgrade {
        #[arg(value_parser=["mimo","codex","claude","cursor","kimi","dsh","codegraph"])]
        tool: String,
        version: Option<String>,
        #[arg(long)]
        force_live: bool,
    },
    Reconcile {
        #[arg(value_parser=["mimo","codex","claude","cursor","kimi","dsh","codegraph"])]
        tool: Option<String>,
    },
}
#[derive(Debug, Subcommand)]
pub enum Manifest {
    Export,
    Plan { file: PathBuf },
    Replay { file: PathBuf, id: String },
}
#[derive(Debug, Subcommand)]
pub enum Config {
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        refresh: bool,
    },
    #[command(about = "用已登记的 chezmoi 纳管一个现有普通文件")]
    Adopt {
        target: PathBuf,
    },
    #[command(about = "停止管理一个 chezmoi 配置文件并保留实际文件")]
    Forget {
        target: String,
    },
    Enroll,
    SourcePath {
        target: Option<PathBuf>,
    },
    Status {
        targets: Vec<PathBuf>,
    },
    Verify {
        #[arg(required = true)]
        targets: Vec<PathBuf>,
    },
    Where {
        query: String,
    },
    Diff {
        path: Option<PathBuf>,
    },
    Apply {
        target: String,
    },
    Restore {
        operation_id: String,
    },
    Reconcile {
        id: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum App {
    #[command(alias = "ls")]
    List,
    Info {
        name: String,
    },
    Check {
        name: Option<String>,
    },
    Upgrade {
        #[arg(value_parser=["mimo","codex","claude","cursor","kimi","dsh","codegraph"])]
        tool: String,
        version: Option<String>,
        #[arg(long)]
        force_live: bool,
    },
}
#[derive(Debug, Subcommand)]
pub enum Toolchain {
    #[command(alias = "ls")]
    List {
        manager: Option<String>,
    },
    Info {
        manager: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum Service {
    #[command(alias = "ls")]
    List,
    Check {
        name: Option<String>,
    },
    Start {
        name: String,
    },
    Stop {
        name: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum Credential {
    #[command(alias = "ls")]
    List,
    Check {
        profile: Option<crate::policy::metadata::Profile>,
    },
    Add {
        profile: crate::policy::metadata::Profile,
        #[arg(value_parser=crate::policy::metadata::alias)]
        alias: String,
    },
    Activate {
        profile: crate::policy::metadata::Profile,
        #[arg(value_parser=crate::policy::metadata::alias)]
        alias: String,
    },
}
