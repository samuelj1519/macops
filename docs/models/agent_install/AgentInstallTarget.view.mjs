const labels = {list:"列出档案", register:"登记已确认的配置", confirm:"确认安装位置", reconcile:"核对待完成的安装", ready:"检查原范围", native:"运行原生安装步骤", checking:"核对原结果", waiting:"等原进程结束", cancelled:"已取消", skipped:"保留现有插件", refused:"显示原因和下一步", publishReady:"确认隔离安装结果", publishing:"写入选定档案", publishPaused:"等待核对写入进度", complete:"插件已安装"};
export const title = "安装到选定的 Agent 档案";
export const state = ({s}) => labels[s.phase];
export const step = (action) => ({
  Choose:"选择档案", Register:"保存已确认的位置", Cancel:"退出", Confirm:"确认登记和安装", Reconcile:"待核对安装已处理或无需核对", Blocked:"原操作范围不符或结果待定",
  Freeze:"保存待写入计划", Apply:"核对并写入", Interrupt:"写入中断", Resume:"核对后继续", Drift:null, Refuse:"目录或设置已经改变", Start:"通过写入检查", Exit:"原步骤退出", LoseAnswer:"结果未收到",
  OriginalExit:"等到原步骤退出", Verify:"确认现有结果", Indeterminate:"无法确认结果", SkipExisting:"已有同名插件或市场", Done:"停留"
})[action] ?? null;
