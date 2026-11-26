export const title = "接管并更新已有共享技能";
const labels = {
    menu: "选择共享技能",
    "confirm-adopt": "确认接管",
    adopting: "保存管理记录",
    "check-version": "核对目标版本",
    "confirm-upgrade": "确认更新",
    upgrading: "更新技能",
    blocked: "说明路径及处理办法",
    updated: "更新完成",
    cancelled: "保留当前版本",
    current: "已是目标版本",
    stopped: "列出结果并停止批次",
};
export const state = ({ s }) => labels[s.phase];
export const step = (action) => ({
    Review: "按现有安装状态选择操作",
    Cancel: "取消",
    Adopt: "确认登记",
    FinishAdopt: "全部接管成功",
    CheckVersion: "核对完成",
    Upgrade: "确认写入",
    FinishUpgrade: "核验结果",
    Stop: "失败或结果待核对",
}[action] ?? null);
