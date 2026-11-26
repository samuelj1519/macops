export const title = "批量能力操作";
export const state = ({ authorized, finished, active }) =>
    finished ? "显示结果" : !authorized ? "确认目标" : active ? "核对原生命令结果" : "检查下一项";
export const step = (action) => ({
    Confirm: "用户确认",
    Cancel: "用户取消",
    Start: "核对范围后执行",
    Observe: "验证结果后释放当前安装",
    Finish: "没有剩余可执行目标",
}[action] ?? null);
