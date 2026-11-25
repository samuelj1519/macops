const labels = {
  start: "收到请求", confirm: "等待用户确认", check: "核对原范围",
  cancelled: "已取消", blocked: "停止并说明原因", running: "处理批量中的目标",
  done: "显示结果",
};
export const title = "恢复、诊断和批量维护";
export const state = ({s}) => labels[s.phase];
export const step = (action) => ({
  Offer: "显示计划", Approve: "确认", Cancel: "取消", Manual: "指定原操作",
  Recover: "核对原结果", Blocked: "发现范围不同", NothingPending: "无需核对",
  Diagnose: "运行诊断", FirstMember: "开始第一项", Stop: "结果待查或取消",
  NextMember: "继续下一项", Followup: "还有后续步骤",
})[action] ?? null;
