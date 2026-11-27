const labels = {
    old: "旧来源",
    declared: "已声明新来源",
    bound: "已核对新市场",
    updated: "已达到目标",
};
export const title = "Claude Code 换源并升级";
export const state = ({state, run}) => run.pending === "none" ? labels[state] : "核对原生结果";
export const step = (action) => ({
    Confirm: "确认换源", Cancel: "取消",
    StartBind: "写入声明并登记市场", StartUpgrade: "执行升级",
    Observe: "核对后继续", Finish: "结束",
}[action] ?? null);
