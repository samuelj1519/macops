const label = {
  running: "查询运行中", inspect: "组长退出，核对辅助进程", cancel: "终止本次查询",
  waiting: "超时后仍等待", done: "记录结果",
};
export const title = "原生查询的超时与退出";
export const state = ({s}) => label[s.phase];
export const step = (action) => ({
  ParentExit: "主程序退出", PeerExit: "辅助进程退出", Finish: "已核对退出",
  Expire: "超过时限", Terminate: "终止原进程组", AbortObservation: "无法核对进程组",
  LatePeer: "辅助进程最终退出",
})[action] ?? null;
