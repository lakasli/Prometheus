<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref } from "vue";
import { api, ApiError, errorMessage } from "../api";
import type { CommitmentScope } from "../context-management";
import { displayTime, localInput, localTimestamp } from "../life-systems";
import {
  restAdmissionLabels,
  restPhaseLabels,
  restResultLabels,
  restStepLabels,
} from "../rest-management";
import type {
  RestControlReceipt,
  RestCycleSummary,
  RestGrant,
  RestPolicy,
  RestPolicyView,
  RestProgress,
  RestStatus,
} from "../rest-management";

const emit = defineEmits<{ unauthorized: [] }>();
const panel = ref<HTMLElement | null>(null);
const status = ref<RestStatus | null>(null),
  policy = ref<RestPolicyView | null>(null);
const progress = ref<RestProgress | null>(null),
  history = ref<RestCycleSummary[]>([]),
  historyCursor = ref<number | null>(null);
const sourceItems = ref<CommitmentScope[]>([]),
  sourceCursor = ref<number | string | null>(null),
  sourceKey = ref("");
const grant = ref<RestGrant | null>(null);
const busy = ref(false),
  error = ref(""),
  statusError = ref(""),
  notice = ref("");
const policyStale = ref(false),
  grantStale = ref(false),
  controlStale = ref(false);
const originalPolicy = ref(""),
  originalGrant = ref("");
const originalGrantExpiry = ref("");
const form = reactive<RestPolicy>({
  anticipation_enabled: false,
  reset_context_on_sleep: false,
  natural_enabled: false,
  schedule_enabled: false,
  time_zone: "Asia/Shanghai",
  sleep_local: null,
  wake_local: null,
  default_sleep_seconds: null,
  max_sleep_seconds: 86400,
  drain_seconds: 30,
  consolidation_seconds: 300,
  queue_limit: 128,
  max_job_attempts: 3,
  max_cycle_calls: 8,
  max_cycle_reserved_tokens: 24000,
  ack_seconds: 30,
});
const sourceForm = reactive({ can_sleep: false, can_wake: false, expiry: "" });
const grantZone = ref("Asia/Shanghai");
const requestKeys = new Map<string, string>();
let disposed = false;
let controller: AbortController | undefined;
const body = computed<RestPolicy>(() => ({
  ...form,
  sleep_local: form.sleep_local || null,
  wake_local: form.wake_local || null,
  default_sleep_seconds:
    form.default_sleep_seconds === null ||
    String(form.default_sleep_seconds) === ""
      ? null
      : Number(form.default_sleep_seconds),
}));
const policyDirty = computed(
  () => !!policy.value && JSON.stringify(body.value) !== originalPolicy.value,
);
const grantDirty = computed(
  () => !!grant.value && JSON.stringify(sourceForm) !== originalGrant.value,
);
const hasUnsaved = computed(
  () => busy.value || policyDirty.value || grantDirty.value,
);
defineExpose({ hasUnsaved });
const selectedSource = computed(() =>
  sourceItems.value.find((item) => item.scope_ref === sourceKey.value),
);
const zone = computed(() => policy.value?.policy.time_zone || "Asia/Shanghai");
const sleepBlocked = computed(() => {
  if (!status.value) return "尚未读取主机状态，新的睡眠已阻断。";
  if (statusError.value)
    return "状态刷新尚未完成，请重新读取后再请求入睡；唤醒仍可使用。";
  if (
    !status.value.clock.host_sample_available ||
    status.value.clock.trusted_for_new_sleep !== true
  )
    return "主机时钟尚未获确认，新的睡眠已阻断；唤醒仍可使用。";
  if (status.value.state.phase !== "awake")
    return "当前周期尚未结束，不重复入睡或延长原唤醒期限。";
  if (controlStale.value) return "状态或提交结果需要重新读取核对。";
  if (
    !policy.value?.policy.wake_local &&
    !policy.value?.policy.default_sleep_seconds
  )
    return "请先保存默认睡眠时长或本地唤醒时刻。";
  return "";
});
const validation = computed(() => {
  const p = body.value;
  for (const [value, min, max, name] of [
    [p.max_sleep_seconds, 1, 86400, "最长睡眠"],
    [p.drain_seconds, 0, 120, "排空窗口"],
    [p.consolidation_seconds, 0, 1800, "整理窗口"],
    [p.queue_limit, 1, 512, "等待队列"],
    [p.max_job_attempts, 1, 3, "每项尝试"],
    [p.max_cycle_calls, 1, 24, "每周期调用"],
    [p.max_cycle_reserved_tokens, 1, 96000, "每周期预留"],
    [p.ack_seconds, 1, 120, "控制回执窗口"],
  ] as [number, number, number, string][]) {
    if (!Number.isInteger(value) || value < min || value > max)
      return `${name}须为 ${min}–${max} 的整数。`;
  }
  if (
    p.default_sleep_seconds !== null &&
    (!Number.isInteger(p.default_sleep_seconds) ||
      p.default_sleep_seconds < 1 ||
      p.default_sleep_seconds > p.max_sleep_seconds)
  )
    return "默认睡眠时长须为正整数，并且不超过最长睡眠。";
  try {
    new Intl.DateTimeFormat("zh-CN", { timeZone: p.time_zone });
  } catch {
    return "请填写有效的 IANA 时区。";
  }
  if (
    [p.sleep_local, p.wake_local].some(
      (value) => value !== null && !/^([01]\d|2[0-3]):[0-5]\d$/.test(value),
    )
  )
    return "本地时刻须为 HH:MM。";
  if (p.schedule_enabled && !p.sleep_local)
    return "启用定时作息前，请填写入睡时刻。";
  if (p.natural_enabled && p.drain_seconds === 0)
    return "开启对话控制时，排空窗口必须大于零。";
  return "";
});
const grantValidation = computed(() => {
  if (!sourceForm.expiry) return "请填写明确的控制许可截止时刻。";
  if (
    sourceForm.expiry === originalGrantExpiry.value &&
    grant.value?.permissions.valid_until
  )
    return "";
  try {
    localTimestamp(sourceForm.expiry, grantZone.value);
    return "";
  } catch {
    return "控制许可期限无效；夏令时缺失或歧义时刻不能使用。";
  }
});
const counts = computed(() => {
  const p = progress.value;
  return {
    steps: p?.steps.length || 0,
    jobs: p?.jobs.length || 0,
    unknown:
      (p?.steps.filter((item) => item.status === "unknown").length || 0) +
      (p?.jobs.filter((item) => item.status === "unknown").length || 0),
  };
});
function assignPolicy(value: RestPolicyView) {
  policy.value = value;
  Object.assign(form, value.policy);
  originalPolicy.value = JSON.stringify(body.value);
  policyStale.value = false;
  if (grant.value)
    grant.value.natural_control_enabled = value.policy.natural_enabled;
}
function assignGrant(value: RestGrant) {
  grant.value = value;
  grantZone.value = zone.value;
  Object.assign(sourceForm, {
    can_sleep: value.permissions.can_sleep,
    can_wake: value.permissions.can_wake,
    expiry:
      value.permissions.valid_until > 0
        ? localInput(value.permissions.valid_until, grantZone.value)
        : "",
  });
  originalGrant.value = JSON.stringify(sourceForm);
  originalGrantExpiry.value = sourceForm.expiry;
  grantStale.value = false;
}
function failed(
  caught: unknown,
  area: "read" | "policy" | "grant" | "control",
  writing: boolean,
) {
  if (caught instanceof ApiError && caught.status === 401) emit("unauthorized");
  const conflict = caught instanceof ApiError && caught.status === 409;
  const uncertain =
    writing && (!(caught instanceof ApiError) || caught.status >= 500);
  if (conflict || uncertain) {
    if (area === "policy") policyStale.value = true;
    if (area === "grant") grantStale.value = true;
    if (area === "control") controlStale.value = true;
  }
  if (
    caught instanceof ApiError &&
    caught.code === "rest_clock_untrusted" &&
    status.value
  ) {
    status.value.clock.trusted_for_new_sleep = false;
    controlStale.value = false;
  }
  error.value =
    caught instanceof ApiError && caught.code === "rest_clock_untrusted"
      ? "主机时钟尚未获确认，未受理新的睡眠；唤醒仍可使用。请重新读取状态。"
      : conflict
        ? "状态、版本或幂等请求已变化。你的填写仍保留，请重新载入对应内容后核对；不会自动重试。"
        : uncertain
          ? "提交结果尚未确认，请重新读取核对；当前操作不会自动重发。"
          : caught instanceof ApiError &&
              [403, 404].includes(caught.status) &&
              area === "grant"
            ? "该来源当前不可控制、已失效或无权管理。请重新读取来源；不会改用其他身份。"
            : errorMessage(caught);
  if (
    caught instanceof ApiError &&
    [403, 404].includes(caught.status) &&
    area === "grant"
  ) {
    grantStale.value = true;
  }
}
async function perform(
  area: "read" | "policy" | "grant" | "control",
  action: (signal: AbortSignal) => Promise<void>,
  writing = false,
) {
  if (busy.value || disposed) return;
  const focus = document.activeElement;
  const hadFocus = !!panel.value?.contains(focus);
  busy.value = true;
  error.value = "";
  notice.value = "";
  controller = new AbortController();
  try {
    await action(controller.signal);
  } catch (caught) {
    if (!disposed) failed(caught, area, writing);
  } finally {
    if (!disposed) {
      busy.value = false;
      await nextTick();
      if (
        !disposed &&
        hadFocus &&
        panel.value &&
        !panel.value.contains(document.activeElement)
      )
        panel.value.focus();
    }
  }
}
async function readStatus(signal: AbortSignal) {
  const [state, current] = await Promise.all([
    api<RestStatus>("/rest", { signal }),
    api<{ current: RestProgress | null }>("/rest/cycles/current", { signal }),
  ]);
  if (disposed) return;
  if (!state.state || !state.clock || !Number.isInteger(state.state.generation))
    throw new Error("invalid_rest_status");
  status.value = state;
  progress.value = current.current;
  controlStale.value = false;
  statusError.value = "";
}
function refreshStatus() {
  void perform("read", async (signal) => {
    try {
      await readStatus(signal);
    } catch (caught) {
      if (!disposed)
        statusError.value =
          "状态读取失败，现有内容为上次采样，不能据此确认最新阶段。";
      throw caught;
    }
  });
}
function reloadPolicy() {
  if (busy.value) return;
  if (
    policyDirty.value &&
    !window.confirm("重新载入会放弃尚未保存的作息策略修改，确定继续？")
  )
    return;
  void perform("policy", async (signal) => {
    const value = await api<RestPolicyView>("/rest/policy", { signal });
    if (!disposed) assignPolicy(value);
  });
}
function requestKey(operation: string, request: unknown, revision: number) {
  const identity = JSON.stringify([operation, revision, request]);
  if (!requestKeys.has(identity)) {
    if (requestKeys.size >= 32)
      requestKeys.delete(requestKeys.keys().next().value!);
    requestKeys.set(identity, crypto.randomUUID());
  }
  return requestKeys.get(identity)!;
}
function savePolicy() {
  if (
    !policy.value ||
    !policyDirty.value ||
    policyStale.value ||
    validation.value
  )
    return;
  const value = { ...body.value },
    revision = policy.value.revision;
  void perform(
    "policy",
    async (signal) => {
      const result = await api<RestPolicyView>("/rest/policy", {
        method: "PUT",
        body: value,
        revision,
        idempotencyKey: requestKey("policy", value, revision),
        signal,
      });
      if (!disposed) {
        assignPolicy(result);
        notice.value =
          "作息策略已独立保存，仅用于后续新周期；当前周期沿用原策略与原唤醒期限。";
      }
    },
    true,
  );
}
function control(kind: "sleep" | "wake") {
  if (
    busy.value ||
    !status.value ||
    (kind === "sleep" && sleepBlocked.value) ||
    (kind === "wake" && controlStale.value)
  )
    return;
  if (
    !window.confirm(
      kind === "sleep"
        ? "按已保存策略请求入睡？现有物理请求将保留，按原截止排空。"
        : "请求唤醒当前周期？在途请求和未知结果仍保留，不会因此提前释放占用。",
    )
  )
    return;
  const generation = status.value.state.generation,
    request = kind === "sleep" ? { wake_at: null } : {};
  void perform(
    "control",
    async (signal) => {
      const result = await api<RestControlReceipt>(`/rest/${kind}`, {
        method: "POST",
        body: request,
        revision: generation,
        idempotencyKey: requestKey(kind, request, generation),
        signal,
      });
      if (disposed) return;
      status.value = { ...status.value!, state: result.state };
      notice.value =
        "控制请求已有宿主回执，下方显示当前阶段；原操作回执不等于当前阶段，也不代表物理占用已释放。";
      try {
        await readStatus(signal);
      } catch {
        if (!disposed)
          statusError.value =
            "控制已有回执，但刷新未完成；请重新读取状态核对实际进度。";
      }
    },
    true,
  );
}
async function readGrant(signal: AbortSignal) {
  if (!selectedSource.value) return;
  const result = await api<RestGrant>(
    `/rest/control-grants/${encodeURIComponent(selectedSource.value.run_id)}`,
    { signal },
  );
  if (!disposed) assignGrant(result);
}
function loadSources(append = false) {
  if (busy.value) return;
  if (
    !append &&
    grantDirty.value &&
    !window.confirm("重新读取来源会放弃尚未保存的控制许可修改，确定继续？")
  )
    return;
  void perform("grant", async (signal) => {
    const result = await api<{
      items: CommitmentScope[];
      next_cursor: number | string | null;
    }>(
      `/commitments/scopes?limit=20${append && sourceCursor.value !== null ? `&after=${encodeURIComponent(String(sourceCursor.value))}` : ""}`,
      { signal },
    );
    if (disposed) return;
    sourceItems.value = append
      ? [
          ...sourceItems.value,
          ...result.items.filter(
            (item) =>
              !sourceItems.value.some(
                (old) => old.scope_ref === item.scope_ref,
              ),
          ),
        ]
      : result.items;
    sourceCursor.value = result.next_cursor;
    if (!append) {
      sourceKey.value = sourceItems.value[0]?.scope_ref || "";
      grant.value = null;
      await readGrant(signal);
    }
  });
}
function switchSource(event: Event) {
  if (busy.value) return;
  if (
    grantDirty.value &&
    !window.confirm("切换来源会放弃尚未保存的控制许可修改，确定继续？")
  ) {
    (event.target as HTMLSelectElement).value = sourceKey.value;
    return;
  }
  sourceKey.value = (event.target as HTMLSelectElement).value;
  grant.value = null;
  void perform("grant", readGrant);
}
function reloadGrant() {
  if (busy.value) return;
  if (
    grantDirty.value &&
    !window.confirm("重新载入会放弃尚未保存的控制许可修改，确定继续？")
  )
    return;
  void perform("grant", readGrant);
}
function saveGrant() {
  if (
    !grant.value ||
    !selectedSource.value ||
    !grantDirty.value ||
    grantStale.value ||
    grantValidation.value
  )
    return;
  const route = selectedSource.value.run_id,
    revision = grant.value.revision;
  const request = {
    can_sleep: sourceForm.can_sleep,
    can_wake: sourceForm.can_wake,
    valid_until:
      sourceForm.expiry === originalGrantExpiry.value
        ? grant.value.permissions.valid_until
        : localTimestamp(sourceForm.expiry, grantZone.value),
  };
  void perform(
    "grant",
    async (signal) => {
      const result = await api<RestGrant>(
        `/rest/control-grants/${encodeURIComponent(route)}`,
        {
          method: "PUT",
          body: request,
          revision,
          idempotencyKey: requestKey(`grant:${route}`, request, revision),
          signal,
        },
      );
      if (!disposed) {
        assignGrant(result);
        notice.value =
          "本来源的睡眠与唤醒控制许可已保存；没有增加资料、模型出域或外发许可。";
      }
    },
    true,
  );
}
function loadHistory(append = false) {
  void perform("read", async (signal) => {
    const value = await api<{
      items: RestCycleSummary[];
      next_cursor: number | null;
    }>(
      `/rest/cycles?limit=20${append && historyCursor.value !== null ? `&after=${historyCursor.value}` : ""}`,
      { signal },
    );
    if (!disposed) {
      history.value = append ? [...history.value, ...value.items] : value.items;
      historyCursor.value = value.next_cursor;
    }
  });
}
function viewCycle(id: string) {
  void perform("read", async (signal) => {
    const value = await api<RestProgress>(
      `/rest/cycles/${encodeURIComponent(id)}`,
      { signal },
    );
    if (!disposed) progress.value = value;
  });
}
function beforeUnload(event: BeforeUnloadEvent) {
  if (hasUnsaved.value) {
    event.preventDefault();
    event.returnValue = "";
  }
}
onMounted(() => {
  window.addEventListener("beforeunload", beforeUnload);
  void perform("read", async (signal) => {
    const [, value] = await Promise.all([
      readStatus(signal),
      api<RestPolicyView>("/rest/policy", { signal }),
    ]);
    if (!disposed) assignPolicy(value);
  });
});
onUnmounted(() => {
  disposed = true;
  controller?.abort();
  window.removeEventListener("beforeunload", beforeUnload);
});
</script>

<template>
  <section
    ref="panel"
    class="rest-panel"
    aria-label="作息与整理"
    tabindex="-1"
    :aria-busy="busy"
  >
    <div class="rest-heading">
      <div>
        <h3>作息与整理</h3>
        <p>查看真实阶段、原定期限与整理依据，分别管理策略和会话控制许可。</p>
      </div>
      <button type="button" :disabled="busy" @click="refreshStatus">
        刷新作息状态
      </button>
    </div>
    <p v-if="error" role="alert" class="error-banner">{{ error }}</p>
    <p v-if="notice" role="status" class="notice-banner">{{ notice }}</p>
    <p v-if="statusError" role="alert" class="error-banner">
      {{ statusError }}
    </p>
    <template v-if="status">
      <dl class="rest-facts">
        <div>
          <dt>实际阶段</dt>
          <dd>
            {{ restPhaseLabels[status.state.phase] || status.state.phase }} ·
            generation {{ status.state.generation }} · 状态 v{{
              status.state.revision
            }}
          </dd>
        </div>
        <div>
          <dt>主机采样</dt>
          <dd>
            {{
              status.clock.sampled_wall_at == null
                ? "尚未提供可靠采样时刻"
                : displayTime(status.clock.sampled_wall_at, zone)
            }}
          </dd>
        </div>
        <div>
          <dt>原定唤醒</dt>
          <dd>{{ displayTime(status.state.wake_at, zone) }}（{{ zone }}）</dd>
        </div>
        <div>
          <dt>排空截止</dt>
          <dd>{{ displayTime(status.state.drain_at, zone) }}</dd>
        </div>
        <div>
          <dt>整理截止</dt>
          <dd>{{ displayTime(status.state.consolidate_at, zone) }}</dd>
        </div>
        <div>
          <dt>物理占用</dt>
          <dd>{{ status.physical_requests }} 项实际请求或投递尚未回收</dd>
        </div>
        <div v-if="status.admission.receiver">
          <dt>入站准入</dt>
          <dd>
            {{
              restAdmissionLabels[status.admission.receiver.decision] ||
              status.admission.receiver.decision
            }}
          </dd>
        </div>
        <div v-if="status.admission.automatic">
          <dt>自动执行准入</dt>
          <dd>
            {{
              restAdmissionLabels[status.admission.automatic.decision] ||
              status.admission.automatic.decision
            }}
          </dd>
        </div>
        <div v-if="status.state.reason">
          <dt>状态依据</dt>
          <dd>{{ status.state.reason }}</dd>
        </div>
      </dl>
      <p class="muted">
        本页显示时区为 {{ zone }}；改变显示时区不修改当前周期原期限。
        状态为上次读取的宿主事实。入站继续接收；新的普通执行与待发消息由宿主按阶段、队列和原平台窗口决定。睡眠、唤醒和逻辑取消都不会抹去
        sending 或 unknown。
      </p>
      <p v-if="sleepBlocked" class="notice-banner">{{ sleepBlocked }}</p>
      <div class="rest-actions">
        <button
          type="button"
          :disabled="busy || !!sleepBlocked"
          @click="control('sleep')"
        >
          按已保存策略入睡</button
        ><button
          type="button"
          :disabled="busy || controlStale"
          @click="control('wake')"
        >
          请求唤醒
        </button>
      </div>
    </template>
    <form v-if="policy" aria-label="作息策略" @submit.prevent="savePolicy">
      <h4>作息策略 · v{{ policy.revision }}</h4>
      <p class="muted">
        每日固定时刻独立保存。提前一分钟可选择如期、取消本次或推迟；推迟每次最多30分钟、每期最多两次。通知失败时按原定时刻执行。
      </p>
      <fieldset :disabled="busy">
        <div class="rest-checks">
          <label
            ><input
              v-model="form.natural_enabled"
              type="checkbox"
              aria-label="允许对话控制作息"
            />允许明确对话控制（另须来源许可）</label
          ><label
            ><input
              v-model="form.schedule_enabled"
              type="checkbox"
              aria-label="启用定时作息"
            />启用定时作息</label
          >
          <label
            ><input
              v-model="form.anticipation_enabled"
              type="checkbox"
              aria-label="提前一分钟选择日程"
            />提前一分钟选择如期、取消或推迟</label
          >
          <label
            ><input
              v-model="form.reset_context_on_sleep"
              type="checkbox"
              aria-label="睡眠时重置聊天上下文"
            />睡眠时重置聊天上下文，记忆从日记读取</label
          >
        </div>
        <div class="rest-fields">
          <label class="field"
            ><span>作息时区</span
            ><input
              v-model="form.time_zone"
              aria-label="作息时区"
              autocomplete="off" /></label
          ><label class="field"
            ><span>本地入睡时刻</span
            ><input
              v-model="form.sleep_local"
              aria-label="本地入睡时刻"
              type="time" /></label
          ><label class="field"
            ><span>本地唤醒时刻</span
            ><input
              v-model="form.wake_local"
              aria-label="本地唤醒时刻"
              type="time" /></label
          ><label class="field"
            ><span>默认睡眠时长（秒，可空）</span
            ><input
              v-model.number="form.default_sleep_seconds"
              aria-label="默认睡眠时长"
              type="number"
              min="1"
              :max="form.max_sleep_seconds" /></label
          ><label class="field"
            ><span>最长睡眠（秒）</span
            ><input
              v-model.number="form.max_sleep_seconds"
              aria-label="最长睡眠"
              type="number"
              min="1"
              max="86400" /></label
          ><label class="field"
            ><span>排空窗口（秒）</span
            ><input
              v-model.number="form.drain_seconds"
              aria-label="排空窗口"
              type="number"
              min="0"
              max="120" /></label
          ><label class="field"
            ><span>整理窗口（秒）</span
            ><input
              v-model.number="form.consolidation_seconds"
              aria-label="整理窗口"
              type="number"
              min="0"
              max="1800" /></label
          ><label class="field"
            ><span>普通等待队列上限</span
            ><input
              v-model.number="form.queue_limit"
              aria-label="普通等待队列上限"
              type="number"
              min="1"
              max="512" /></label
          ><label class="field"
            ><span>每项最多尝试</span
            ><input
              v-model.number="form.max_job_attempts"
              aria-label="每项最多尝试"
              type="number"
              min="1"
              max="3" /></label
          ><label class="field"
            ><span>每周期最多整理调用</span
            ><input
              v-model.number="form.max_cycle_calls"
              aria-label="每周期最多整理调用"
              type="number"
              min="1"
              max="24" /></label
          ><label class="field"
            ><span>每周期保守预留 token</span
            ><input
              v-model.number="form.max_cycle_reserved_tokens"
              aria-label="每周期保守预留"
              type="number"
              min="1"
              max="96000"
            /><small>预留不是精确计费；失败或未知结果不退回预算。</small></label
          ><label class="field"
            ><span>控制回执窗口（秒）</span
            ><input
              v-model.number="form.ack_seconds"
              aria-label="控制回执窗口"
              type="number"
              min="1"
              max="120"
          /></label>
        </div>
      </fieldset>
      <p v-if="validation" role="status" class="error-banner">
        {{ validation }}
      </p>
      <div class="rest-actions">
        <button
          type="submit"
          :disabled="busy || policyStale || !policyDirty || !!validation"
        >
          保存作息策略</button
        ><button type="button" :disabled="busy" @click="reloadPolicy">
          重新载入作息策略
        </button>
      </div>
    </form>
    <div class="rest-subsection" aria-label="会话作息控制许可">
      <h4>会话控制许可</h4>
      <p class="muted">
        只从当前账号可管理且已有来源证明的会话选择，分别允许入睡或唤醒，并设置期限。这不授予资料读取、模型出域、发送或主动联系许可。普通晚安或结束聊天不等于作息指令。
      </p>
      <div class="rest-actions">
        <button type="button" :disabled="busy" @click="loadSources(false)">
          读取可管理来源</button
        ><button
          v-if="sourceCursor !== null"
          type="button"
          :disabled="busy"
          @click="loadSources(true)"
        >
          更多来源
        </button>
      </div>
      <label v-if="sourceItems.length" class="field"
        ><span>控制来源</span
        ><select
          :value="sourceKey"
          aria-label="作息控制来源"
          :disabled="busy"
          @change="switchSource"
        >
          <option
            v-for="item in sourceItems"
            :key="item.scope_ref"
            :value="item.scope_ref"
          >
            {{ item.title }} · {{ item.source_label }} ·
            {{ displayTime(item.updated_at) }} ·
            {{ item.scope_ref.slice(0, 8) }}
          </option>
        </select></label
      >
      <form v-if="grant" aria-label="来源控制许可" @submit.prevent="saveGrant">
        <p>
          许可 v{{ grant.revision }} ·
          {{
            !grant.grant_present
              ? "尚未授权"
              : grant.grant_expired
                ? "已到期"
                : "已保存"
          }}；对话控制全局{{
            grant.natural_control_enabled ? "已开启" : "仍关闭"
          }}。
        </p>
        <fieldset :disabled="busy">
          <div class="rest-checks">
            <label
              ><input
                v-model="sourceForm.can_sleep"
                type="checkbox"
                aria-label="本来源可请求入睡"
              />可请求入睡</label
            ><label
              ><input
                v-model="sourceForm.can_wake"
                type="checkbox"
                aria-label="本来源可请求唤醒"
              />可请求唤醒</label
            >
          </div>
          <label class="field"
            ><span>控制许可截止（{{ grantZone }}）</span
            ><input
              v-model="sourceForm.expiry"
              aria-label="控制许可截止"
              type="datetime-local"
            /><small
              >由服务器核验是否仍有效且不超过 366
              天；不使用浏览器当前时间。取消两个勾选可撤销控制，记录期限仍须有效。</small
            ></label
          >
        </fieldset>
        <p v-if="grantValidation" role="status" class="muted">
          {{ grantValidation }}
        </p>
        <div class="rest-actions">
          <button
            type="submit"
            :disabled="busy || grantStale || !grantDirty || !!grantValidation"
          >
            保存本来源控制许可</button
          ><button type="button" :disabled="busy" @click="reloadGrant">
            重新载入本来源许可
          </button>
        </div>
      </form>
    </div>
    <div class="rest-subsection" aria-label="整理进度">
      <h4>整理进度与检查点</h4>
      <p v-if="!progress" class="muted">
        当前没有可展示的周期进度。可读取最近周期；未读取不代表整理已完成。
      </p>
      <template v-else
        ><p>
          周期 generation {{ progress.generation }} · 原定唤醒
          {{ displayTime(progress.wake_at, zone) }} ·
          {{
            progress.partial
              ? "部分结果，保留未完成与未知"
              : "未标记部分结果，不等于全部成功"
          }}
        </p>
        <p>
          本次返回 {{ counts.steps }} 个步骤 · {{ counts.jobs }} 个作业 ·
          {{ counts.unknown }} 项结果未知
        </p>
        <p v-if="progress.error_code" class="error-banner">
          周期依据：{{ progress.error_code }}
        </p>
        <ol class="rest-progress">
          <li v-for="step in progress.steps" :key="step.kind">
            <strong
              >{{ restStepLabels[step.kind] || step.kind }}：{{
                restResultLabels[step.status] || step.status
              }}</strong
            ><span>已处理 {{ step.checkpoint.processed_refs }} 个引用</span
            ><code v-if="step.checkpoint.cursor_digest"
              >游标摘要 {{ step.checkpoint.cursor_digest }}</code
            ><code v-if="step.result_digest"
              >结果摘要 {{ step.result_digest }}</code
            ><span v-if="step.error_code">依据 {{ step.error_code }}</span>
          </li>
        </ol>
        <ul class="rest-progress">
          <li v-for="job in progress.jobs" :key="job.id">
            <strong
              >作业 {{ job.id }}：{{
                restResultLabels[job.status] || job.status
              }}</strong
            ><span>尝试 {{ job.attempts }} 次 · fence {{ job.fence }}</span
            ><span v-if="job.error_code">依据 {{ job.error_code }}</span>
          </li>
        </ul></template
      >
      <p class="muted">
        进度仅含元数据。这里不能完成步骤、重试作业、执行命令或释放请求占用。完整睡眠的运行与平台行为仍须按固定版本分别验收。
      </p>
      <div class="rest-actions">
        <button type="button" :disabled="busy" @click="loadHistory(false)">
          读取最近周期</button
        ><button
          v-if="historyCursor !== null"
          type="button"
          :disabled="busy"
          @click="loadHistory(true)"
        >
          更早周期
        </button>
      </div>
      <ul class="rest-history">
        <li v-for="item in history" :key="item.cycle_id">
          <span
            >generation {{ item.generation }} · 策略 v{{
              item.policy_revision
            }}
            · 原唤醒 {{ displayTime(item.wake_at, zone) }}</span
          ><span
            >{{ item.model_calls }} 次整理调用 · 保守预留
            {{ item.reserved_tokens }} token ·
            {{ item.partial ? "部分结果" : "未标记部分结果" }}</span
          ><button
            type="button"
            :disabled="busy"
            @click="viewCycle(item.cycle_id)"
          >
            查看此周期进度
          </button>
        </li>
      </ul>
    </div>
  </section>
</template>

<style scoped>
.rest-panel {
  border-top: 1px solid var(--line);
  margin-top: 24px;
  padding-top: 24px;
  min-width: 0;
}
.rest-heading,
.rest-actions {
  display: flex;
  gap: 12px;
  align-items: center;
  justify-content: space-between;
  flex-wrap: wrap;
}
.rest-heading h3 {
  margin: 0;
}
.rest-heading p,
.muted {
  font-size: 12px;
  line-height: 1.8;
}
.rest-facts {
  font-size: 12px;
  display: grid;
  gap: 8px;
}
.rest-facts div {
  display: flex;
  gap: 12px;
}
.rest-facts dt {
  color: var(--muted);
  min-width: 70px;
}
.rest-facts dd {
  margin: 0;
  overflow-wrap: anywhere;
}
.rest-actions {
  justify-content: flex-start;
  margin: 12px 0;
}
.rest-panel fieldset {
  border: 0;
  margin: 0;
  padding: 0;
  min-width: 0;
}
.rest-fields {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}
.rest-checks {
  display: flex;
  gap: 16px;
  flex-wrap: wrap;
  margin: 16px 0;
}
.rest-checks label {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  font-size: 12px;
  line-height: 1.8;
}
.rest-checks input {
  width: auto;
  flex: none;
  margin-top: 4px;
}
.rest-panel form,
.rest-subsection {
  border-top: 1px solid var(--line);
  padding-top: 12px;
  margin-top: 20px;
}
.rest-progress,
.rest-history {
  padding-left: 20px;
  font-size: 12px;
  line-height: 1.8;
  display: grid;
  gap: 12px;
}
.rest-progress li,
.rest-history li {
  min-width: 0;
  overflow-wrap: anywhere;
}
.rest-progress span,
.rest-progress code,
.rest-history span {
  display: block;
}
.rest-panel select {
  max-width: 100%;
}
.rest-panel code {
  white-space: normal;
}
.rest-panel h4 {
  margin: 12px 0;
}
@media (max-width: 650px) {
  .rest-fields {
    grid-template-columns: 1fr;
  }
  .rest-facts div {
    display: block;
  }
  .rest-facts dt {
    margin-bottom: 3px;
  }
  .rest-actions button {
    max-width: 100%;
  }
}
</style>
