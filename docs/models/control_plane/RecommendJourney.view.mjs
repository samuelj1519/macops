export const title = "推荐、安装与核对";
const labels = {
  start: "打开命令", advice: "查看来源依据", picker: "选择安装项", confirm: "核对安装范围",
  cancelled: "取消", blocked: "查看候选及阻断原因", running: "执行已确认的计划",
  complete: "安装完成", failed: "安装失败", unknown: "按原操作号核对",
  "resume-visible": "查看继续命令", "resume-hidden": "缺少继续命令", "setup-done": "查看或设置推荐模型"
};
export const state = ({s}) => s.phase === "blocked" && !s.shown ? "候选被隐藏" : labels[s.phase];
const steps = {
  Setup: "进入独立设置", AdviceOnly: "暂无可执行候选", ShowOptions: "核对来源与候选",
  InvalidNumber: "输入无效编号", Cancel: "返回或取消", Select: "选择有效编号",
  Reject: "发现已有待核对操作", Confirm: "确认并通过锁内校验", Finish: "查看原操作结果",
  Recover: "显式核对原操作"
};
export const step = (action) => steps[action] ?? null;
