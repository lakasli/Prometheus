export function shiftMonth(day: string, amount: number) {
  const date = new Date(`${day}T12:00:00Z`);
  const originalDay = date.getUTCDate();
  date.setUTCDate(1);
  date.setUTCMonth(date.getUTCMonth() + amount);
  const last = new Date(
    Date.UTC(date.getUTCFullYear(), date.getUTCMonth() + 1, 0),
  ).getUTCDate();
  date.setUTCDate(Math.min(originalDay, last));
  return date.toISOString().slice(0, 10);
}

export function blockReason(value: string) {
  const reasons: Record<string, string> = {
    video_provider_unavailable: "视频理解尚未验收，活动暂不执行",
    contact_consent_and_verified_delivery_required:
      "等待接收许可与真实投递能力验收",
    consolidation_system_job_only: "整理由系统日记作业统一执行",
    action_not_registered: "需要已注册并获授权的动作",
    model_budget_exhausted: "本日模型预算已用完，等待后续安排",
    autonomy_budget_exhausted: "本日自主活动预算已用完",
    resource_revoked: "资料授权已撤销",
    resource_unavailable_or_out_of_scope: "资料不可用或不在授权范围内",
    grant_revoked_expired_or_changed: "自主授权已撤销、过期或变更",
    autonomy_revoked: "自主授权已撤销",
    execution_window_elapsed: "执行时间窗已结束，已完成片段会保留",
    resource_already_read: "这份资料已经读完，本次已跳过",
    restart_checkpoint: "重启后从已保存的片段继续",
  };
  return reasons[value] || value;
}
