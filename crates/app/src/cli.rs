use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "macops",
    bin_name = "mo",
    version,
    about = "本机软件、Agent 资产与目录的统一维护入口",
    long_about = "直接运行 mo 浏览本机，或用 mo recommend 名称 查看安装方式。用 mo help 命令 子命令 查看可用操作和示例。"
)]
pub struct Cli {
    #[arg(help_heading = "高级选项", long = "timeout", alias = "timeout-secs", value_name = "SECONDS", global = true,
        default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..=3600),
        help = "每次运行外部程序最多等多少秒")]
    pub timeout_secs: u64,
    #[arg(
        help_heading = "脚本选项",
        long,
        global = true,
        help = "输出结构化 JSON"
    )]
    pub json: bool,
    #[arg(long, global = true, help = "预演，不执行安装器或持久业务写入")]
    pub dry_run: bool,
    #[arg(
        help_heading = "脚本选项",
        long,
        global = true,
        help = "跳过交互确认，仍执行所有校验"
    )]
    pub yes: bool,
    #[arg(long, global = true, action = clap::ArgAction::Count, help = "显示安装时的原生工具诊断")]
    pub verbose: u8,
    #[arg(
        help_heading = "高级选项",
        long,
        global = true,
        help = "高级用法：选择另一用户目录下的安装与配置"
    )]
    pub home: Option<PathBuf>,
    #[arg(
        help_heading = "高级选项",
        long,
        global = true,
        help = "高级用法：指定本机设置所在目录"
    )]
    pub settings: Option<PathBuf>,
    #[arg(
        help_heading = "高级选项",
        long,
        global = true,
        help = "macops 自有配置目录"
    )]
    pub config_dir: Option<PathBuf>,
    #[arg(
        help_heading = "高级选项",
        long,
        global = true,
        help = "macops 自有持久数据目录"
    )]
    pub data_dir: Option<PathBuf>,
    #[arg(
        help_heading = "高级选项",
        long,
        global = true,
        help = "高级用法：指定操作历史和恢复记录所在目录"
    )]
    pub state_dir: Option<PathBuf>,
    #[arg(
        help_heading = "高级选项",
        long,
        global = true,
        help = "高级用法：指定临时下载和版本索引所在目录"
    )]
    pub cache_dir: Option<PathBuf>,
    #[arg(
        help_heading = "高级选项",
        long,
        global = true,
        help = "显式 catalog JSON；缺失或损坏时失败"
    )]
    pub catalog: Option<PathBuf>,
    #[arg(
        help_heading = "高级选项",
        long,
        global = true,
        help = "明确选择 chezmoi 原生 source owner"
    )]
    pub chezmoi_source: Option<PathBuf>,
    #[arg(
        help_heading = "高级选项",
        long,
        global = true,
        help = "明确选择 chezmoi 原生 destination"
    )]
    pub chezmoi_destination: Option<PathBuf>,
    #[arg(
        help_heading = "高级选项",
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
        about = "查看和维护本机软件",
        before_help = "按任务使用\n  mo app                              浏览本机软件\n  mo app NAME                         查看版本、位置和来源\n  mo app install NAME                 安装并接管\n  mo app adopt NAME                   接管已有安装\n  mo app upgrade NAME --dry-run       先看升级计划\n  mo app uninstall NAME --dry-run     核对要卸载的实例\n  mo app service list                 找到可启动或停止的服务"
    )]
    App {
        #[arg(
            long,
            global = true,
            value_name = "PATH",
            help = "按项目目录查看资源（只读）"
        )]
        project: Option<PathBuf>,
        #[command(subcommand)]
        command: Option<AppCmd>,
    },
    #[command(
        about = "查看和同步由 chezmoi 管理的配置",
        before_help = "按任务使用\n  mo config                             查看配置文件\n  mo config enroll                      选择并登记使用的 chezmoi 配置\n  mo config diff [PATH]                 看待应用的差异\n  mo config apply PATH --dry-run       看将应用到哪里\n  mo config adopt /absolute/path       接管已有文件\n  mo config forget PATH                停止管理，保留本机文件"
    )]
    Config {
        #[arg(
            long,
            global = true,
            value_name = "PATH",
            help = "按项目目录查看资源（只读）"
        )]
        project: Option<PathBuf>,
        #[command(subcommand)]
        command: Option<Config>,
    },
    #[command(
        about = "技能：技能与插件的安装、升级与检查",
        before_help = "按任务使用\n  mo skill --details                       查看技能、来源与接管状态\n  mo recommend owner/repo                 查看 GitHub 安装方式\n  mo skill install owner/repo              安装并接管 GitHub 技能\n  mo skill adopt NAME                      接管已有安装\n  mo skill refresh --help                  刷新来源索引\n  mo skill outdated                        查看可用更新\n  mo skill upgrade --help                  选择并预演升级\n  mo skill uninstall --help                卸载已接管技能",
        after_help = "其他维护入口\n  mo skill market                          浏览插件市场\n  mo skill plugins --help                  核对客户端插件\n  mo skill sync --all --dry-run             预演技能与档案同步\n  mo skill policy --help                   查看策略和恢复入口"
    )]
    Skill {
        #[arg(
            long,
            global = true,
            value_name = "PATH",
            help = "按项目目录查看资源（只读）"
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
        about = "查看、升级和启动 Agent",
        before_help = "按任务使用\n  mo agent                              查看本机 Agent 配置\n  mo agent tools                        核对各 CLI 的版本\n  mo agent add NAME --config PATH       登记自定义 Agent\n  mo agent install codex                安装 CLI\n  mo agent upgrade codex --dry-run     看升级影响\n  mo agent launch PROFILE               启动指定配置\n  mo agent uninstall PROFILE --dry-run 先看将归档哪个单独配置\n  mo agent credential list             查看凭据状态"
    )]
    Agent {
        #[arg(
            long,
            global = true,
            value_name = "PATH",
            help = "按项目目录查看资源（只读）"
        )]
        project: Option<PathBuf>,
        #[command(subcommand)]
        command: Option<AgentCmd>,
    },
    #[command(
        about = "看本机状态、安装方式和维护入口",
        before_help = "示例\n  mo recommend jq               找软件的安装来源\n  mo recommend owner/repo       查看技能或插件的安装方式\n  mo recommend --setup          设置或测试推荐模型"
    )]
    Recommend {
        #[arg(value_name = "NAME_OR_REPO", help = "软件名称、GitHub 仓库名称或链接")]
        product: Option<String>,
        #[arg(
            long,
            value_name = "FILE",
            help = "高级用法：读取已有的本机来源说明文件"
        )]
        evidence: Option<PathBuf>,
        #[arg(
            long,
            conflicts_with = "evidence",
            help = "读取已明确选定的公开 GitHub 仓库"
        )]
        github: Option<String>,
        #[arg(long, help = "使用 DeepSeek 解释随程序提供的公开证据")]
        model: bool,
        #[arg(
            long,
            requires = "model",
            help = "高级用法：为推荐模型指定旧钥匙串账户"
        )]
        keychain_account: Option<String>,
        #[arg(long, conflicts_with_all = ["product", "evidence", "github", "model"], help = "配置推荐模型与连接")]
        setup: bool,
    },
    #[command(
        about = "检查上次操作的结果",
        before_help = "示例\n  mo recover                    查看待核对的操作\n  mo recover ID --dry-run       查看原计划\n  mo recover ID                 核对原结果。重跑日常维护命令时也会先自动核对"
    )]
    Recover {
        #[arg(
            value_name = "ID",
            help = "从 mo recover 列表中取得的操作编号。只查看列表时省略。"
        )]
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
    History {
        #[arg(help = "用对象名、目标 ID 或操作号筛选")]
        query: Option<String>,
    },
    #[command(about = "查询任意对象；按名称或 ID 跨类型搜索")]
    Search {
        #[arg(help = "本机对象的名称或 ID")]
        query: String,
    },
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
        #[arg(value_name = "NAME_OR_ID", help = "应用名称或详情页显示的唯一 ID")]
        id: String,
        #[arg(long, help = "指定要安装的版本。可用版本由安装来源决定。")]
        version: Option<String>,
    },
    #[command(about = "升级应用；省略对象时先刷新索引再升级全部可升级项")]
    Upgrade {
        #[arg(
            value_name = "NAME_OR_ID",
            help = "指定一个应用。省略时处理全部可升级软件。"
        )]
        id: Option<String>,
        #[arg(long, requires = "id", help = "为指定应用选择版本。")]
        version: Option<String>,
    },
    #[command(about = "卸载明确对象，保留配置与用户数据")]
    Uninstall {
        #[arg(
            value_name = "NAME_OR_ID",
            help = "要卸载的应用。存在同名安装时使用详情页的 ID。"
        )]
        id: String,
    },
    #[command(about = "按名称或 ID 将本机已有安装纳入管理，省略目标可交互选择")]
    Adopt {
        #[arg(value_name = "NAME_OR_ID", group = "adopt_target")]
        target: Option<String>,
        #[arg(long, value_name = "ID", group = "adopt_target")]
        id: Option<String>,
        #[arg(
            long,
            requires = "adopt_target",
            help = "高级用法：把已有安装关联到本机配置中已授权的维护规则"
        )]
        bind_recipe: bool,
    },
    #[command(about = "检查本地安装，损坏输入报告 unknown")]
    Doctor { id: Option<String> },
    #[command(about = "启动已声明的服务")]
    Start { id: String },
    #[command(hide = true, about = "通过原生管理器发现安装；不自动接管")]
    Discover { manager: Option<String> },
    #[command(about = "刷新软件的可用版本，不改动已有安装")]
    Refresh { id: Option<String> },
    #[command(about = "用已查询的版本信息查看哪些软件可升级")]
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

#[derive(Clone, Debug, Args)]
pub struct SkillSelection {
    #[arg(help = "技能名、GitHub 仓库或唯一 ID。一个仓库可包含多项技能。用 --only 分别选择多项。")]
    pub name: Option<String>,
    #[arg(
        long,
        help = "指定某个提交的完整 SHA。按标签或分支更新某个仓库时可用 OWNER/REPO@版本。"
    )]
    pub revision: Option<String>,
    #[arg(long = "only", help = "指定一个成员或来源分组，可重复")]
    pub only: Vec<String>,
    #[arg(long, help = "处理全部符合条件的技能。先加 --dry-run 核对范围。")]
    pub all: bool,
    #[arg(long, help = "仅用于旧策略同步。值来自原操作记录。")]
    pub change_id: Option<String>,
    #[command(flatten)]
    pub agents: SkillAgents,
}

#[derive(Clone, Debug, Default, Args)]
pub struct SkillAgents {
    #[arg(
        long = "agent",
        value_name = "NAME",
        help = "选择一个 Agent；可重复，all 选择全部适用的 Agent"
    )]
    pub names: Vec<String>,
    #[arg(long, help = "同意修改共享技能目录，所有读取该目录的 Agent 都会受影响")]
    pub shared: bool,
}

#[derive(Debug, Subcommand)]
pub enum SkillCmd {
    #[command(
        about = "从 GitHub 安装技能并接管后续更新",
        before_help = "示例\n  mo skill install owner/repo --dry-run          先查看有哪些技能和安装位置\n  mo skill install owner/repo                   选择技能并安装\n  mo skill install owner/repo@v1.2.0            使用指定版本\n  mo skill install owner/repo --agent claude-mi --agent claude-glm\n  mo recommend owner/repo                      查看插件或其他安装方式"
    )]
    Install {
        #[arg(
            value_name = "GITHUB_REPO",
            required_unless_present = "recipe",
            conflicts_with = "recipe",
            help = "GitHub 仓库名称或链接，可加 @标签、分支或提交。安装到用户共享技能目录。"
        )]
        source: Option<String>,
        #[arg(
            long,
            value_name = "FILE",
            help = "高级用法：读取已有的本机配方文件（绝对路径）"
        )]
        recipe: Option<PathBuf>,
        #[command(flatten)]
        agents: SkillAgents,
    },
    #[command(
        hide = true,
        about = "接管本机已有的 GitHub 技能，保留原安装",
        before_help = "示例\n  mo skill --details                          查找已有技能和来源\n  mo skill adopt NAME --dry-run                查看可以接管的位置\n  mo skill adopt owner/repo                   接管一个来源里的已有技能"
    )]
    Adopt {
        #[arg(help = "已有技能的名称、GitHub 仓库或唯一安装 ID")]
        id: String,
        #[arg(
            long,
            value_name = "FILE",
            help = "高级用法：指定已有的本机配方文件（绝对路径）"
        )]
        recipe: Option<PathBuf>,
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
        #[command(flatten)]
        agents: SkillAgents,
    },
    #[command(about = "在选定的 Agent 中启用已安装插件")]
    Enable {
        id: String,
        #[command(flatten)]
        agents: SkillAgents,
    },
    #[command(about = "在选定的 Agent 中停用已安装插件")]
    Disable {
        id: String,
        #[command(flatten)]
        agents: SkillAgents,
    },
    #[command(
        about = "按技能名或 GitHub 仓库更新本机技能",
        before_help = "示例\n  mo skill upgrade NAME --dry-run                    预演一个成员的升级\n  mo skill upgrade source:owner/repo --dry-run       预演已登记来源的升级\n  mo skill upgrade --only NAME --only OTHER --dry-run  预演多个成员\n  mo skill upgrade NAME                              更新已接管的技能\n  mo skill upgrade owner/repo@v1.2.0                  更新到指定版本\n\n  mo skill upgrade owner/repo --agent all --dry-run    查看各 Agent 的安装和目标版本\n\n固定版本用 @ 指定。脚本写入共享目录时加 --shared。"
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
    #[command(about = "登记已有 Agent 的命令和配置目录")]
    Add {
        #[arg(value_name = "COMMAND")]
        name: String,
        #[arg(long, value_name = "ABSOLUTE_PATH")]
        config: PathBuf,
        #[arg(long = "type", value_parser = ["claude", "codex", "cursor"], help = "指定无法从命令名识别的客户端类型")]
        client: Option<String>,
    },
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
        #[arg(long, help = "在这套独立 Codex 配置中选择一个已有的原生 profile")]
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
    #[command(about = "查看 Agent CLI 的本机和已固定版本")]
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
    Adopt { target: PathBuf },
    #[command(about = "停止管理一个 chezmoi 配置文件并保留实际文件")]
    Forget { target: String },
    #[command(about = "登记当前由 chezmoi 管理的文件和位置")]
    Enroll,
    #[command(about = "查看配置的管理源路径")]
    SourcePath { target: Option<PathBuf> },
    #[command(about = "查看管理源和本机文件是否一致")]
    Status {
        #[arg(help = "要检查的路径。省略时查看全部已登记配置。")]
        targets: Vec<PathBuf>,
    },
    #[command(about = "核验选定文件是否与管理源一致")]
    Verify {
        #[arg(required = true, help = "一个或多个要核验的文件路径")]
        targets: Vec<PathBuf>,
    },
    #[command(about = "查找配置及其实际路径")]
    Where {
        #[arg(help = "文件名或路径片段")]
        query: String,
    },
    #[command(about = "查看应用配置前会有哪些改动")]
    Diff {
        #[arg(help = "只比较这个路径。省略时比较已登记配置。")]
        path: Option<PathBuf>,
    },
    #[command(about = "把管理源的配置应用到一个本机文件")]
    Apply {
        #[arg(help = "配置路径或清单中的唯一 ID")]
        target: String,
    },
    #[command(about = "按原记录还原支持还原的配置操作")]
    Restore {
        #[arg(help = "从 mo history 中取得原操作编号")]
        operation_id: String,
    },
    #[command(about = "对照本机和管理源中固定的工具版本")]
    Reconcile {
        #[arg(help = "指定工具。省略时只查看差异清单。")]
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
