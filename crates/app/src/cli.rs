use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "macops",
    bin_name = "mo",
    version,
    about = "本机软件、Agent 资产与目录的统一维护入口",
    long_about = "统一文法：mo <类型> [对象] [动作]。无对象即列表，带对象即详情；安装、升级、卸载各类型同名。写命令默认执行，使用 --dry-run 预演。未知来源和恢复状态不会自动跳过。"
)]
pub struct Cli {
    #[arg(long = "timeout", alias = "timeout-secs", value_name = "SECONDS", global = true,
        default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..=3600),
        help = "每次运行外部程序最多等多少秒")]
    pub timeout_secs: u64,
    #[arg(long, global = true, help = "输出结构化 JSON")]
    pub json: bool,
    #[arg(long, global = true, help = "预演，不执行安装器或持久业务写入")]
    pub dry_run: bool,
    #[arg(long, global = true, help = "跳过交互确认，仍执行所有校验")]
    pub yes: bool,
    #[arg(long, global = true, action = clap::ArgAction::Count, help = "显示安装时的原生工具诊断")]
    pub verbose: u8,
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

#[derive(Debug, Args)]
pub struct Detail {
    pub target: String,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(
        about = "应用：列表、详情、安装、升级、卸载与纳入管理；终端环境自动进入浏览",
        after_help = "按任务使用  mo app adopt [名称] 查找已有安装并纳入管理\n  mo app refresh / mo app outdated 查看可用版本\n  mo app <名称> upgrade --dry-run 预演升级\n  mo app service list 查看已声明的服务"
    )]
    App {
        #[arg(
            long,
            global = true,
            value_name = "PATH",
            help = "按项目目录过滤资源快照"
        )]
        project: Option<PathBuf>,
        #[command(subcommand)]
        command: Option<AppCmd>,
    },
    #[command(
        about = "配置：chezmoi 归属、状态与治理",
        after_help = "按任务使用  mo config enroll 登记配置范围\n  mo config list --refresh 刷新受管路径\n  mo config diff 查看差异方向\n  mo config apply <路径> --dry-run 预演应用"
    )]
    Config {
        #[arg(
            long,
            global = true,
            value_name = "PATH",
            help = "按项目目录过滤资源快照"
        )]
        project: Option<PathBuf>,
        #[command(subcommand)]
        command: Option<Config>,
    },
    #[command(
        about = "技能：技能与插件的安装、升级与检查",
        before_help = "按任务使用\n  mo skill --details                       查看技能、来源与接管状态\n  mo recommend owner/repo                 查看 GitHub 安装方式\n  mo skill install --help                  用本机配方安装\n  mo skill adopt --help                    接管已有安装\n  mo skill refresh --help                  刷新来源索引\n  mo skill outdated                        查看可用更新\n  mo skill upgrade --help                  选择并预演升级\n  mo skill uninstall --help                卸载已接管技能",
        after_help = "其他维护入口\n  mo skill market                          浏览插件市场\n  mo skill plugins --help                  核对客户端插件\n  mo skill sync --all --dry-run             预演技能与档案同步\n  mo skill policy --help                   查看策略和恢复入口"
    )]
    Skill {
        #[arg(
            long,
            global = true,
            value_name = "PATH",
            help = "按项目目录过滤资源快照"
        )]
        project: Option<PathBuf>,
        #[arg(long, help = "显示每项技能的详细证据")]
        details: bool,
        #[arg(long = "only", value_name = "NAME", help = "仅列出指定技能")]
        only: Vec<String>,
        #[command(subcommand)]
        command: Option<SkillCmd>,
    },
    #[command(
        about = "Agent 与 Harness：CLI 工具、档案、凭据与启动",
        after_help = "按任务使用  mo agent tools 查看 CLI 版本与固定版本\n  mo agent launch <档案> --dry-run 查看启动范围\n  mo agent credential list 查看凭据状态"
    )]
    Agent {
        #[arg(
            long,
            global = true,
            value_name = "PATH",
            help = "按项目目录过滤资源快照"
        )]
        project: Option<PathBuf>,
        #[command(subcommand)]
        command: Option<AgentCmd>,
    },
    #[command(about = "智能安装：查询官方渠道并给出可执行计划")]
    Recommend {
        product: Option<String>,
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
        #[arg(long, conflicts_with_all = ["product", "evidence", "github", "model"], help = "配置推荐模型与连接")]
        setup: bool,
    },
    #[command(about = "检查上次操作的结果。省略操作号时显示待检查的操作")]
    Recover {
        operation_id: Option<String>,
        #[arg(
            long = "mark-failed",
            alias = "resolve-failed",
            requires = "operation_id",
            help = "把支持的未完成操作记为失败。保留当前安装和设置"
        )]
        resolve_failed: bool,
    },
    #[command(about = "查看操作历史，可按操作号或目标 ID 查询")]
    History { query: Option<String> },
    #[command(about = "查询任意对象；按名称或 ID 跨类型搜索")]
    Search { query: String },
    #[command(
        name = "migrate",
        hide = true,
        about = "一次性转换旧治理数据；保留旧源并校验分叉"
    )]
    Migrate {
        #[command(subcommand)]
        command: Migrate,
    },
    #[command(hide = true, about = "导出本机管理清单或预演已有清单")]
    Manifest {
        #[command(subcommand)]
        command: Manifest,
    },
    #[command(name = "dirs", hide = true, about = "查询准确目录与用途")]
    Dirs {
        id: Option<String>,
        #[arg(
            long,
            conflicts_with = "id",
            help = "查看 macops 自有目录与实际配置路径"
        )]
        roots: bool,
    },
    #[command(
        name = "cleanup",
        hide = true,
        about = "清理 macops 自有缓存；原生共享缓存只提供建议"
    )]
    Cleanup { id: String },
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
    NativeWorker {
        journal: PathBuf,
        #[arg(long)]
        capture_progress: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum AppCmd {
    #[command(about = "安装官方渠道声明的应用并自动纳入管理")]
    Install {
        id: String,
        #[arg(long)]
        version: Option<String>,
    },
    #[command(about = "升级应用；省略对象时先刷新索引再升级全部可升级项")]
    Upgrade {
        id: Option<String>,
        #[arg(long, requires = "id")]
        version: Option<String>,
    },
    #[command(about = "卸载明确对象，保留配置与用户数据")]
    Uninstall { id: String },
    #[command(about = "按名称或 ID 将本机已有安装纳入管理，省略目标可交互选择")]
    Adopt {
        #[arg(value_name = "NAME_OR_ID", group = "adopt_target")]
        target: Option<String>,
        #[arg(long, value_name = "ID", group = "adopt_target")]
        id: Option<String>,
        #[arg(long, requires = "adopt_target")]
        bind_recipe: bool,
    },
    #[command(about = "检查本地安装，损坏输入报告 unknown")]
    Doctor { id: Option<String> },
    #[command(about = "启动已声明的服务")]
    Start { id: String },
    #[command(hide = true, about = "通过原生管理器发现安装；不自动接管")]
    Discover { manager: Option<String> },
    #[command(hide = true, about = "刷新可用版本索引，不升级软件")]
    Refresh { id: Option<String> },
    #[command(hide = true, about = "根据已有索引显示可更新或未知项")]
    Outdated { id: Option<String> },
    #[command(hide = true, about = "应用资源的统一快照")]
    Resources,
    #[command(hide = true, about = "已声明服务查询")]
    Service {
        #[command(subcommand)]
        command: ServiceCmd,
    },
    #[command(about = "停止已声明的服务")]
    Stop { id: String },
    #[command(hide = true, name = "__detail")]
    Detail(Detail),
    #[command(external_subcommand)]
    Target(Vec<String>),
}

#[derive(Clone, Copy)]
pub(crate) struct AdoptTarget<'a> {
    pub key: &'a str,
    pub exact: bool,
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
    #[arg(help = "成员名称、唯一 ID 或已登记的 source:OWNER/REPO 分组；也可用 --only 选多个")]
    pub name: Option<String>,
    #[arg(long, help = "指定来源提交的完整 SHA")]
    pub revision: Option<String>,
    #[arg(long = "only", help = "指定一个成员或来源分组，可重复")]
    pub only: Vec<String>,
    #[arg(long, help = "选择全部符合维护条件的成员")]
    pub all: bool,
    #[arg(long, help = "旧策略同步的变更 ID；原生技能维护不接受")]
    pub change_id: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum SkillCmd {
    #[command(
        about = "按固定配方安装技能",
        before_help = "示例\n  mo recommend owner/repo                          查看 GitHub 仓库的安装方式\n  mo skill install --recipe /path/recipe.json --dry-run  预演本机配方\n  mo skill install --recipe /path/recipe.json      按配方安装"
    )]
    Install {
        #[arg(
            long,
            value_name = "ABSOLUTE_JSON_PATH",
            help = "本机技能配方 JSON 文件的绝对路径。GitHub 仓库可用 mo recommend 查看安装说明"
        )]
        recipe: PathBuf,
    },
    #[command(
        about = "按固定配方将已有技能纳入管理",
        before_help = "示例\n  mo skill --details                                   查找待接管的技能\n  mo skill adopt NAME --recipe /path/recipe.json --dry-run  预演接管\n  mo skill adopt NAME --recipe /path/recipe.json        登记已有安装"
    )]
    Adopt {
        #[arg(help = "已有原生安装的名称或唯一安装 ID")]
        id: String,
        #[arg(
            long,
            value_name = "ABSOLUTE_JSON_PATH",
            help = "本机技能配方 JSON 文件的绝对路径"
        )]
        recipe: PathBuf,
    },
    #[command(
        name = "uninstall",
        alias = "remove",
        about = "卸载已纳入管理的技能",
        before_help = "示例\n  mo skill uninstall NAME --dry-run  预演卸载\n  mo skill uninstall NAME            卸载已接管的技能"
    )]
    Remove {
        #[arg(help = "已接管成员的名称或唯一 ID")]
        id: String,
    },
    #[command(
        about = "升级已接管的技能",
        before_help = "示例\n  mo skill upgrade NAME --dry-run                    预演一个成员的升级\n  mo skill upgrade source:owner/repo --dry-run       预演已登记来源的升级\n  mo skill upgrade --only NAME --only OTHER --dry-run  预演多个成员\n  mo skill upgrade NAME                              执行升级\n\n先用 mo skill refresh NAME 刷新跟踪版本，再用 mo skill outdated NAME 查看结果。"
    )]
    Upgrade(SkillSelection),
    #[command(
        about = "检查本地技能与插件证据",
        before_help = "示例\n  mo skill doctor NAME  查看单个技能的检查结果\n  mo skill doctor       检查本机技能与插件"
    )]
    Doctor {
        #[arg(help = "要检查的技能名称")]
        name: Option<String>,
        #[arg(long, help = "筛选名称，可重复")]
        only: Vec<String>,
    },
    #[command(
        about = "浏览 Codex、Claude Code 和 Cursor 插件市场",
        after_help = "示例  mo skill market claude:mi
      mo skill market cursor --online"
    )]
    Market {
        #[arg(value_name = "HARNESS_OR_PROFILE")]
        profile: Option<String>,
        #[arg(long, requires = "profile")]
        online: bool,
    },
    #[command(hide = true, about = "客户端原生插件的固定安装配方")]
    Plugins {
        #[command(subcommand)]
        command: NativePlugins,
    },
    #[command(hide = true, about = "技能策略与变更恢复")]
    Policy {
        #[command(subcommand)]
        command: Policy,
    },
    #[command(hide = true, about = "同步技能链接与声明")]
    Sync(SkillSelection),
    #[command(
        about = "刷新技能来源索引，不升级",
        before_help = "示例\n  mo skill refresh NAME --dry-run               预演刷新范围\n  mo skill refresh source:owner/repo            刷新已登记来源下的成员"
    )]
    Refresh(SkillSelection),
    #[command(
        about = "根据已有索引显示可更新技能",
        before_help = "示例\n  mo skill outdated           查看现有索引\n  mo skill outdated NAME      查看单个成员\n  mo skill outdated --json    用结构化输出查看原因"
    )]
    Outdated {
        #[arg(help = "成员名称、唯一 ID 或来源分组")]
        name: Option<String>,
        #[arg(long, help = "筛选成员或来源分组，可重复")]
        only: Vec<String>,
    },
    #[command(hide = true, about = "技能资源的统一快照")]
    Resources,
    #[command(hide = true, name = "__detail")]
    Detail(Detail),
    #[command(external_subcommand)]
    Target(Vec<String>),
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
        #[arg(long,value_parser=crate::skills::rules::skills::parse_change_id)]
        change_id: Option<String>,
    },
    Recover {
        #[arg(long,value_parser=crate::skills::rules::skills::parse_change_id)]
        change_id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AgentCmd {
    #[command(
        about = "安装或更新一个 Agent CLI（mimo、codex、claude、cursor、kimi、dsh、codegraph）"
    )]
    Install {
        #[arg(value_parser=crate::agent::ai::parse_tool)]
        tool: String,
        version: Option<String>,
        #[arg(
            long = "local",
            alias = "force-live",
            help = "用于 mise 管理的工具。只改本机安装和本机 mise 设置，保留设置来源"
        )]
        force_live: bool,
    },
    #[command(about = "升级 Agent CLI；省略对象时检查全部工具的配置差异")]
    Upgrade {
        #[arg(value_parser=crate::agent::ai::parse_tool)]
        tool: Option<String>,
        #[arg(
            long = "local",
            alias = "force-live",
            requires = "tool",
            help = "用于 mise 管理的工具。只改本机安装和本机 mise 设置，保留设置来源"
        )]
        force_live: bool,
    },
    #[command(about = "确认并继续已核验的插件安装")]
    Resume { operation_id: String },
    #[command(about = "启动已纳入管理的独立 Agent 档案")]
    Launch {
        profile: String,
        #[arg(long)]
        native_profile: Option<String>,
    },
    #[command(about = "卸载单独配置并将其移入本机归档")]
    Uninstall { profile: String },
    #[command(about = "凭据元信息；永不读取密钥值")]
    Credential {
        #[command(subcommand)]
        command: Credential,
    },
    #[command(hide = true, about = "Agent 档案资源的统一快照")]
    Resources,
    #[command(hide = true, about = "Agent CLI 工具版本清单")]
    Tools,
    #[command(hide = true, name = "__detail")]
    Detail(Detail),
    #[command(external_subcommand)]
    Target(Vec<String>),
}

#[derive(Debug, Subcommand)]
pub enum Manifest {
    Export,
    Plan { file: PathBuf },
    Replay { file: PathBuf, id: String },
}

#[derive(Debug, Subcommand)]
pub enum Config {
    #[command(alias = "ls", about = "刷新并查看配置归属")]
    List {
        #[arg(long)]
        refresh: bool,
    },
    #[command(about = "将现有普通文件纳入已登记的 chezmoi 管理范围")]
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
        id: Option<String>,
    },
    #[command(hide = true, about = "配置资源的统一快照")]
    Resources,
    #[command(hide = true, name = "__detail")]
    Detail(Detail),
    #[command(external_subcommand)]
    Target(Vec<String>),
}

#[derive(Debug, Subcommand)]
pub enum ServiceCmd {
    List,
    Check { name: Option<String> },
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
