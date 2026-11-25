export const title = "查找、预演、维护与核对";
const labels = {
  start: "进入命令", planned: "核对维护计划", cancelled: "取消维护", blocked: "显示阻断原因",
  running: "执行已确认计划", complete: "报告执行结果",
};
export const state = ({s}) => {
  if (s.phase === "preview") return s.showPlan ? "查看目标和原范围" : "计划藏在原始字段中";
  if (s.phase === "stopped") return s.reportedSkipped ? "未知结果与未执行项" : "未知结果，剩余项状态不明";
  if (s.phase === "history") return s.originalFound ? "查看原操作记录" : "找不到原操作号";
  if (s.phase === "native-result") return s.originalFound ? "未知结果与原操作号" : "未知结果缺少操作号";
  if (s.phase === "config") return s.configIncomplete && s.saysClean ? "过滤范围误报完整" : s.configIncomplete ? "配置证据不完整" : "已核验所选配置";
  return labels[s.phase];
};
const labelsForStep = {
  OpenPreview: "请求只读预演", OpenBatch: "选择软件维护", FindHistory: "输入原操作号",
  NativeResult: "查看原生配置应用结果", CheckConfig: "核验配置", Cancel: "取消", Block: "锁内发现待核对操作",
  Dispatch: "确认并通过原校验", Finish: "读到完成证据", Unknown: "原操作结果不明",
  Recover: "用原操作号进入历史"
};
export const step = (action) => labelsForStep[action] ?? null;
