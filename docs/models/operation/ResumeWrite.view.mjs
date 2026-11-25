export const title = "先核对上次结果";
const phase = {
  start: "收到命令", read: "只读查看", wait: "等待上一次操作", blocked: "说明还缺什么", oldDone: "报告上一次已完成",
  newReady: "重新准备本次操作", checking: "检查上一次的记录", newRunning: "执行本次操作", newDone: "报告本次结果"
};
export const state = ({s}) => s.phase === "newRunning" && s.printed ? "显示原程序进度" : phase[s.phase];
export const step = (action) => ({
  Read: "只查看", Ready: "原记录已结束", Review: "发现待检查记录", Wait: "原进程组仍存在",
  Refuse: "范围不同或证据不足", SettleOld: "确认上一次的结果", StartNew: "检查新计划后开始",
  Progress: "原程序输出消息", Finish: "核对完成"
})[action] ?? null;
