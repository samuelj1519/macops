export const title = "Agent 升级受理与原操作核对";
const labels = {
  start: "进入升级或核对", pending: "先核对原操作", drift: "查看配置差异", plan: "可准备升级",
  running: "等待原进程组退出", profileChanged: "Claude 档案待核对",
  evidenceMissing: "原操作证据不足", verified: "原结果通过核验"
};
export const state = ({s}) => labels[s.phase];
export const step = (action) => ({Upgrade: "检查升级条件", CheckRecovery: "核对原操作"})[action] ?? null;
