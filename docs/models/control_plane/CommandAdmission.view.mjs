export const title = "macops 命令受理与操作核对";
const labels = {
  start: "收到命令", worker: "处理原生子进程", settings: "进入推荐设置",
  migration: "进入迁移命令", blocked: "提示先核对迁移", admitted: "已核对迁移状态",
  read: "执行普通命令", selected: "查看历史详情", preview: "预览原操作",
  stale: "拒绝写入", write: "执行确认的动作", done: "退出详情"
};
export const state = ({s}) => {
  if (s.phase === "selected") return s.offered ? "历史项可核对" : "历史项无核对入口";
  if (s.phase === "preview" && s.writer === "maintenance") return "预览维护计划";
  return labels[s.phase];
};
const steps = {
  RunWorker: "原生子进程请求", Setup: "设置推荐模型", Migrate: "查看或处理迁移",
  Admit: "普通命令通过入口检查", Block: "发现未完成迁移", Read: "继续普通命令",
  SelectHistory: "打开历史项", Leave: "退出详情", Preview: "请求核对",
  PreviewWrite: "发起终端维护", Cancel: "取消确认", Confirm: "持写锁核验后确认", Reject: "迁移开始或证据已变化"
};
export const step = (action) => steps[action] ?? null;
