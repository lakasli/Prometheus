<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { api, ApiError, errorMessage } from "../api";
import SkillOutput from "./SkillOutput.vue";
import {
  newTriggerDefinition,
  triggerState,
  triggerSummary,
  triggerDate,
} from "../task-triggers";
import type {
  TriggerCapabilities,
  TriggerDefinition,
  TriggerJob,
  TriggerOccurrence,
  TriggerPreview,
  TriggerRule,
  CalendarRevision,
} from "../task-triggers";

const props = defineProps<{ initialDraft?: TriggerDefinition | null }>();
const emit = defineEmits<{ unauthorized: []; consumed: [] }>();
const prefix = "/task-triggers";
const capabilities = ref<TriggerCapabilities | null>(null);
const jobs = ref<TriggerJob[]>([]);
const total = ref(0);
const offset = ref(0);
const selected = ref<TriggerJob | null>(null);
const form = ref<TriggerDefinition | null>(null);
const baseline = ref("");
const preset = ref("once_after");
const atTimes = ref("09:00:00");
const weekdays = ref("1,2,3,4,5");
const monthDays = ref("last");
const monthDates = ref("02-29");
const dateList = ref("");
const filters = ref("[]");
const onceAt = ref("");
const seconds = ref(30);
const eventChoice = ref("");
const eventCooldown = ref(60);
const eventDayClass = ref<"any" | "workday" | "restday">("any");
const calendarDayClass = ref<"any" | "workday" | "restday">("any");
const actionText = ref("");
const skillChoice = ref("");
const outputMode = ref<"text" | "html_drawing" | "native_image">("text");
const preview = ref<TriggerPreview | null>(null);
const occurrences = ref<TriggerOccurrence[]>([]);
const occurrenceTotal = ref(0);
const occurrenceOffset = ref(0);
const inspected = ref<TriggerOccurrence | null>(null);
const busy = ref(false);
const error = ref("");
const notice = ref("");
const confirmAction = ref<"stop" | "archive" | "test-runs" | null>(null);
const calendarInput = ref("");
const calendarBaseline = ref("");
const calendars = ref<CalendarRevision[]>([]);
const calendarDraft = ref<CalendarRevision | null>(null);
const calendarVerified = ref(false);
const calendarAffected = ref<TriggerJob[]>([]);
const calendarImpactReady = ref(false);
const calendarChanges = computed(() => {
  const draft = calendarDraft.value;
  if (!draft) return null;
  const current = calendars.value.find(
    (item) => item.active && item.document.year === draft.document.year,
  );
  const changes = (before: string[], after: string[]) => ({
    added: after.filter((item) => !before.includes(item)),
    removed: before.filter((item) => !after.includes(item)),
  });
  return {
    previous: current?.revision || null,
    holidays: changes(
      current?.document.holidays || [],
      draft.document.holidays,
    ),
    working: changes(
      current?.document.working_days || [],
      draft.document.working_days,
    ),
  };
});
const controllers = new Set<AbortController>();
let disposed = false;
let selectionEpoch = 0;
type Mutation = {
  path: string;
  body: unknown;
  key: string;
  label: string;
  kind: "job" | "test" | "delivery";
};
const attempt = ref<Mutation | null>(null);
const locked = computed(() => busy.value || !!attempt.value);
const dirty = computed(
  () => !!form.value && serializeForm() !== baseline.value,
);
const hasUnsaved = computed(
  () =>
    dirty.value ||
    !!attempt.value ||
    calendarInput.value !== calendarBaseline.value,
);
const eligible = computed(
  () => !!selected.value && !dirty.value && !locked.value,
);
const canActivate = computed(
  () =>
    eligible.value &&
    !!preview.value?.valid &&
    !!preview.value.preview_token &&
    !preview.value.blocked_reason &&
    selected.value?.lifecycle !== "archived",
);
const activeEvent = computed(() =>
  capabilities.value?.event_types.find(
    (item) => `${item.source_id}|${item.event_type}` === eventChoice.value,
  ),
);
const isModel = computed(() => form.value?.action.type === "assistant_run");

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}
function strings(value: string) {
  return value
    .split(/[\s,，]+/)
    .map((item) => item.trim())
    .filter(Boolean);
}
function serializeForm() {
  return JSON.stringify({
    form: form.value,
    preset: preset.value,
    atTimes: atTimes.value,
    weekdays: weekdays.value,
    monthDays: monthDays.value,
    monthDates: monthDates.value,
    dateList: dateList.value,
    filters: filters.value,
    onceAt: onceAt.value,
    seconds: seconds.value,
    eventChoice: eventChoice.value,
    eventCooldown: eventCooldown.value,
    eventDayClass: eventDayClass.value,
    calendarDayClass: calendarDayClass.value,
    actionText: actionText.value,
    skillChoice: skillChoice.value,
    outputMode: outputMode.value,
  });
}
function hydrate(value: TriggerDefinition, imported = false) {
  form.value = clone(value);
  const rule = value.trigger;
  preset.value =
    rule.type === "once"
      ? `once_${rule.mode}`
      : rule.type === "calendar"
        ? rule.frequency === "daily" &&
          (rule.day_class === "workday" || rule.day_class === "restday")
          ? rule.day_class
          : rule.frequency || "daily"
        : rule.type;
  atTimes.value = (rule.at_times || ["09:00:00"]).join(", ");
  weekdays.value = (rule.weekdays || [1, 2, 3, 4, 5]).join(",");
  monthDays.value = (rule.month_days || ["last"]).join(",");
  monthDates.value = (rule.month_dates || [{ month: 2, day: 29 }])
    .map(
      (item) =>
        `${String(item.month).padStart(2, "0")}-${String(item.day).padStart(2, "0")}`,
    )
    .join(", ");
  dateList.value = (rule.dates || []).join("\n");
  filters.value = JSON.stringify(rule.filters || [], null, 2);
  onceAt.value = rule.at || "";
  seconds.value = rule.after_seconds ?? rule.every_seconds ?? 30;
  eventChoice.value =
    rule.source_id && rule.event_type
      ? `${rule.source_id}|${rule.event_type}`
      : "";
  eventCooldown.value = rule.cooldown_seconds ?? 60;
  eventDayClass.value = rule.day_class || "any";
  calendarDayClass.value = rule.day_class || "any";
  actionText.value = value.action.content ?? value.action.prompt ?? "";
  skillChoice.value = value.action.skill?.id || "";
  outputMode.value = value.action.output_mode || "text";
  baseline.value = imported ? "" : serializeForm();
  preview.value = null;
  inspected.value = null;
  confirmAction.value = null;
}
function makeDefinition(): TriggerDefinition {
  if (!form.value) throw new Error("请先选择或新建任务。");
  const value = clone(form.value);
  if (!value.title.trim() || new TextEncoder().encode(value.title).length > 128)
    throw new Error("任务名称须为 1–128 UTF-8 字节。");
  if (
    !actionText.value.trim() ||
    new TextEncoder().encode(actionText.value).length > 16000
  )
    throw new Error("执行内容须为 1–16,000 UTF-8 字节。");
  let rule: TriggerRule;
  if (preset.value === "once_at")
    rule = { type: "once", mode: "at", at: onceAt.value.trim() };
  else if (preset.value === "once_after")
    rule = { type: "once", mode: "after", after_seconds: seconds.value };
  else if (preset.value === "interval")
    rule = {
      type: "interval",
      every_seconds: seconds.value,
      anchor: "first_enable",
      first_fire: "after_interval",
    };
  else if (preset.value === "event") {
    const event = activeEvent.value;
    if (!event) throw new Error("请选择服务器实际提供的事件。");
    const parsed: unknown = JSON.parse(filters.value);
    if (!Array.isArray(parsed) || parsed.length > 8)
      throw new Error("事件筛选须为最多 8 条条件的 JSON 数组。");
    rule = {
      type: "event",
      source_id: event.source_id,
      event_type: event.event_type,
      filters: parsed,
      cooldown_seconds: eventCooldown.value,
      day_class: eventDayClass.value,
      calendar_id: eventDayClass.value === "any" ? null : "cn-official",
    };
  } else {
    const shortcut = preset.value === "workday" || preset.value === "restday";
    const dayClass = shortcut
      ? (preset.value as "workday" | "restday")
      : calendarDayClass.value;
    const frequency = shortcut
      ? "daily"
      : (preset.value as NonNullable<TriggerRule["frequency"]>);
    rule = {
      type: "calendar",
      frequency,
      at_times: strings(atTimes.value).map((item) =>
        /^\d{2}:\d{2}$/.test(item) ? `${item}:00` : item,
      ),
      day_class: dayClass,
      calendar_id: dayClass === "any" ? null : "cn-official",
    };
    if (frequency === "weekly")
      rule.weekdays = strings(weekdays.value).map(Number);
    if (frequency === "monthly")
      rule.month_days = strings(monthDays.value).map((item) =>
        item === "last" ? "last" : Number(item),
      );
    if (frequency === "yearly")
      rule.month_dates = strings(monthDates.value).map((item) => {
        const [month, day] = item.split("-").map(Number);
        return { month: month!, day: day! };
      });
    if (frequency === "dates") rule.dates = strings(dateList.value);
  }
  value.trigger = rule;
  if (value.action.type === "notify") {
    value.action = { type: "notify", content: actionText.value };
    value.budget.daily_model_runs = 0;
  } else {
    const skill = capabilities.value?.skills.find(
      (item) => item.id === skillChoice.value,
    );
    if (skillChoice.value && !skill)
      throw new Error("所选 Skill 已不可用，请重新选择。");
    value.action = {
      type: "assistant_run",
      prompt: actionText.value,
      skill: skill ? { id: skill.id, revision: skill.revision } : null,
      persona: { mode: "instance_snapshot" },
      output_mode: outputMode.value,
      context_policy: "isolated",
    };
  }
  if (value.policy.misfire === "skip") {
    value.policy.catch_up_window_seconds = 0;
    value.policy.max_catch_up = 1;
  } else if (value.policy.misfire === "coalesce_one")
    value.policy.max_catch_up = 1;
  value.active_window.start_at ||= null;
  value.active_window.end_at ||= null;
  return value;
}
async function request<T>(path: string, opts: Parameters<typeof api>[1] = {}) {
  if (disposed) throw new DOMException("Task editor disposed", "AbortError");
  const controller = new AbortController();
  controllers.add(controller);
  const timer = setTimeout(() => controller.abort(), 15000);
  try {
    return await api<T>(prefix + path, { ...opts, signal: controller.signal });
  } finally {
    clearTimeout(timer);
    controllers.delete(controller);
  }
}
function showError(caught: unknown) {
  if (disposed) return;
  if (caught instanceof ApiError && caught.status === 401) emit("unauthorized");
  else
    error.value =
      caught instanceof Error &&
      !(caught instanceof ApiError) &&
      caught.name !== "AbortError" &&
      !(caught instanceof TypeError)
        ? caught.message
        : errorMessage(caught);
}
async function action(fn: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  try {
    await fn();
  } catch (caught) {
    showError(caught);
  } finally {
    if (!disposed) busy.value = false;
  }
}
async function loadList() {
  const result = await request<{ items: TriggerJob[]; total: number }>(
    `/jobs?limit=20&offset=${offset.value}`,
  );
  if (disposed) return;
  jobs.value = result.items;
  total.value = result.total;
}
async function history() {
  if (!selected.value) return;
  const id = selected.value.id;
  const epoch = selectionEpoch;
  const result = await request<{ items: TriggerOccurrence[]; total: number }>(
    `/jobs/${encodeURIComponent(id)}/occurrences?limit=20&offset=${occurrenceOffset.value}`,
  );
  if (disposed || epoch !== selectionEpoch) return;
  occurrences.value = result.items;
  occurrenceTotal.value = result.total;
  if (inspected.value)
    inspected.value =
      result.items.find((item) => item.id === inspected.value!.id) ||
      inspected.value;
  const latest = await request<TriggerJob>(`/jobs/${encodeURIComponent(id)}`);
  if (disposed || epoch !== selectionEpoch) return;
  jobs.value = jobs.value.map((job) => (job.id === id ? latest : job));
  if (dirty.value || attempt.value) return;
  if (
    JSON.stringify(latest.task_trigger_draft) ===
    JSON.stringify(selected.value?.task_trigger_draft)
  ) {
    if (latest.revision !== selected.value?.revision) preview.value = null;
    selected.value = latest;
  }
}
async function select(job: TriggerJob) {
  if (
    locked.value ||
    (dirty.value && !window.confirm("任务有未保存的修改，确定切换？"))
  )
    return;
  await action(async () => {
    selectionEpoch++;
    const epoch = selectionEpoch;
    const result = await request<TriggerJob>(
      `/jobs/${encodeURIComponent(job.id)}`,
    );
    if (disposed || epoch !== selectionEpoch) return;
    selected.value = result;
    hydrate(result.task_trigger_draft);
    occurrenceOffset.value = 0;
    notice.value = "";
    await history();
  });
}
function create() {
  if (
    locked.value ||
    (dirty.value && !window.confirm("任务有未保存的修改，确定新建？"))
  )
    return;
  selectionEpoch++;
  selected.value = null;
  occurrences.value = [];
  occurrenceTotal.value = 0;
  hydrate(newTriggerDefinition());
  notice.value = "新任务保存后仍为停用；校验预览后再应用并启用。";
}
async function previewRule() {
  if (locked.value) return;
  await action(async () => {
    const definition = makeDefinition();
    const body = {
      task_trigger_draft: definition,
      ...(selected.value && !dirty.value
        ? {
            job_id: selected.value.id,
            expected_revision: selected.value.revision,
            draft_revision: selected.value.draft_revision,
          }
        : {}),
    };
    const result = await request<TriggerPreview>("/preview", {
      method: "POST",
      body,
    });
    if (!disposed) preview.value = result;
  });
}
async function perform(value: Mutation) {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  attempt.value = value;
  let confirmed = false;
  try {
    const result = await request<TriggerJob | TriggerOccurrence>(value.path, {
      method: "POST",
      body: value.body,
      idempotencyKey: value.key,
    });
    if (disposed) return;
    confirmed = true;
    attempt.value = null;
    confirmAction.value = null;
    if (value.kind === "job") {
      selected.value = result as TriggerJob;
      hydrate((result as TriggerJob).task_trigger_draft);
      notice.value = value.label;
    } else {
      inspected.value = result as TriggerOccurrence;
      notice.value = value.label;
    }
    await loadList();
    await history();
  } catch (caught) {
    if (disposed) return;
    if (
      confirmed ||
      (caught instanceof ApiError &&
        caught.status >= 400 &&
        caught.status < 500)
    )
      attempt.value = null;
    showError(caught);
    if (attempt.value)
      error.value =
        "操作结果未确认。请求内容与编号已冻结，请原样重试以核对同一次操作；不会自动重复执行。";
  } finally {
    if (!disposed) busy.value = false;
  }
}
async function save() {
  if (locked.value) return;
  try {
    const definition = makeDefinition();
    await perform({
      path: selected.value
        ? `/jobs/${encodeURIComponent(selected.value.id)}/drafts`
        : "/jobs",
      body: {
        task_trigger_draft: definition,
        ...(selected.value
          ? { expected_revision: selected.value.revision }
          : {}),
      },
      key: crypto.randomUUID(),
      label: selected.value
        ? "草稿已保存，生效版本尚未改变。请重新预览后应用。"
        : "任务已保存为停用草稿，请预览后应用并启用。",
      kind: "job",
    });
  } catch (caught) {
    showError(caught);
  }
}
async function manage(name: string) {
  if (!selected.value || !eligible.value) return;
  const job = selected.value;
  const labels: Record<string, string> = {
    activate: "任务版本已应用并启用。",
    pause: "已暂停新触发；已开始的执行可继续完成。",
    stop: "已请求停止并取消当前执行，请在记录中核对结算。",
    resume: "已恢复未来触发；暂停期间不会积压补跑。",
    archive: "任务已归档，停止未来触发。",
    "test-runs": "手动执行已受理；不会移动正式触发计划。",
  };
  await perform({
    path: `/jobs/${encodeURIComponent(job.id)}/${name}`,
    body: {
      expected_revision: job.revision,
      ...(name === "activate"
        ? {
            draft_revision: job.draft_revision,
            preview_token: preview.value?.preview_token,
          }
        : {}),
    },
    key: crypto.randomUUID(),
    label: labels[name] || "操作已确认。",
    kind: name === "test-runs" ? "test" : "job",
  });
}
async function inspect(item: TriggerOccurrence) {
  await action(async () => {
    const result = await request<TriggerOccurrence>(
      `/occurrences/${encodeURIComponent(item.id)}`,
    );
    if (!disposed) inspected.value = result;
  });
}
async function retryDelivery() {
  if (!inspected.value || locked.value) return;
  await perform({
    path: `/occurrences/${encodeURIComponent(inspected.value.id)}/retry-delivery`,
    body: {},
    key: crypto.randomUUID(),
    label: "已提交既有结果的保存 / 投递重试，不会重新调用模型。",
    kind: "delivery",
  });
}
function changeAction() {
  if (!form.value) return;
  form.value.budget.daily_occurrences =
    form.value.action.type === "assistant_run" ? 100 : 1000;
  form.value.budget.daily_model_runs =
    form.value.action.type === "assistant_run" ? 10 : 0;
  if (
    form.value.action.type === "assistant_run" &&
    form.value.policy.misfire === "bounded_catch_up"
  )
    form.value.policy.misfire = "skip";
}
async function acknowledgeClock() {
  await action(async () => {
    const sampled = await request<TriggerCapabilities>("/capabilities");
    if (disposed) return;
    capabilities.value = sampled;
    if (sampled.clock?.trusted) {
      notice.value = "服务器时钟已经可信，无需重复确认。";
      return;
    }
    if (
      !window.confirm(
        `请核对服务器参考时间：${triggerDate(sampled.now, "UTC")}。确认后仅授予本次进程一小时的时钟信任，不改变主机时间。`,
      )
    )
      return;
    await request("/clock/acknowledge", {
      method: "POST",
      body: { observed_now: sampled.now, confirmed: true },
    });
    const result = await request<TriggerCapabilities>("/capabilities");
    if (!disposed) {
      capabilities.value = result;
      preview.value = null;
      notice.value = "服务器时钟已人工确认，请重新预演后启用任务。";
    }
  });
}
async function loadCalendars() {
  const result = await request<{ items: CalendarRevision[] }>("/calendars");
  if (!disposed) calendars.value = result.items;
}
async function saveCalendarDraft() {
  if (locked.value) return;
  await action(async () => {
    if (
      !calendarInput.value.trim() ||
      new TextEncoder().encode(calendarInput.value).length > 60000
    )
      throw new Error("请粘贴不超过 60,000 UTF-8 字节的官方日历 JSON。");
    try {
      JSON.parse(calendarInput.value);
    } catch {
      throw new Error("日历 JSON 格式无效。");
    }
    await loadCalendars();
    const result = await request<CalendarRevision>("/calendars/drafts", {
      method: "POST",
      file: new Blob([`{"document":${calendarInput.value}}`], {
        type: "application/json",
      }),
      contentType: "application/json",
    });
    if (disposed) return;
    calendarBaseline.value = calendarInput.value;
    calendarDraft.value = result;
    calendarVerified.value = false;
    await loadCalendarImpact();
    if (disposed) return;
    notice.value =
      "日历导入草稿已保存，尚未应用。请核对来源、日期差异及影响后确认。";
  });
}
async function loadCalendarImpact() {
  calendarImpactReady.value = false;
  const own: TriggerJob[] = [];
  for (let start = 0; start < 256; start += 100) {
    const page = await request<{ items: TriggerJob[]; total: number }>(
      `/jobs?limit=100&offset=${start}`,
    );
    own.push(...page.items);
    if (start + 100 >= page.total) break;
  }
  if (disposed) return;
  calendarAffected.value = own.filter(
    (job) =>
      job.lifecycle === "enabled" &&
      job.active_definition?.trigger.calendar_id === "cn-official",
  );
  calendarImpactReady.value = true;
}
async function applyCalendar() {
  if (
    locked.value ||
    !calendarDraft.value ||
    !calendarVerified.value ||
    !calendarImpactReady.value
  )
    return;
  const draft = calendarDraft.value;
  if (
    !window.confirm(
      `将应用 ${draft.document.year} 年中国日历 v${draft.revision}。这会影响全实例未来的工作日 / 休息日判断；已受理机会保留旧版本。确认已核对官方来源与日期？`,
    )
  )
    return;
  await action(async () => {
    await request(`/calendars/${encodeURIComponent(draft.id)}/apply`, {
      method: "POST",
      body: { expected_digest: draft.sha256 },
      idempotencyKey: crypto.randomUUID(),
    });
    const result = await request<TriggerCapabilities>("/capabilities");
    if (disposed) return;
    capabilities.value = result;
    calendarDraft.value = null;
    calendarVerified.value = false;
    preview.value = null;
    await loadCalendars();
    await loadList();
    notice.value =
      "已核验日历版本已应用。未受理的未来机会使用新版本，旧执行记录保持原版本。";
  });
}
function beforeUnload(event: BeforeUnloadEvent) {
  if (hasUnsaved.value) {
    event.preventDefault();
    event.returnValue = "";
  }
}
function inspectCalendar(calendar: CalendarRevision) {
  if (
    locked.value ||
    (calendarInput.value !== calendarBaseline.value &&
      !window.confirm("日历 JSON 有未保存修改，确定切换版本？"))
  )
    return;
  calendarInput.value = JSON.stringify(calendar.document, null, 2);
  calendarBaseline.value = calendarInput.value;
  calendarDraft.value = calendar.active ? null : calendar;
  calendarVerified.value = false;
  void action(loadCalendarImpact);
}
watch(calendarInput, () => {
  if (calendarInput.value !== calendarBaseline.value) {
    calendarDraft.value = null;
    calendarVerified.value = false;
  }
});
watch(
  () => serializeForm(),
  () => {
    preview.value = null;
    confirmAction.value = null;
  },
);
onMounted(() => {
  window.addEventListener("beforeunload", beforeUnload);
  if (props.initialDraft) {
    hydrate(props.initialDraft, true);
    emit("consumed");
    notice.value =
      "这是模型生成的任务草稿。请检查所有内容并预演；保存后仍停用。";
  }
  void action(async () => {
    const result = await request<TriggerCapabilities>("/capabilities");
    if (disposed) return;
    capabilities.value = result;
    await loadList();
  });
});
onUnmounted(() => {
  window.removeEventListener("beforeunload", beforeUnload);
  disposed = true;
  selectionEpoch++;
  for (const controller of controllers) controller.abort();
  controllers.clear();
});
defineExpose({ hasUnsaved });
</script>

<template>
  <div class="task-trigger-workspace">
    <aside class="panel trigger-library" aria-label="触发任务列表">
      <div class="panel-heading">
        <div>
          <h2>触发任务</h2>
          <p>设置规则，让助手适时处理任务。</p>
        </div>
        <span class="badge neutral">{{ total }}</span>
      </div>
      <div class="trigger-actions">
        <button
          class="button primary small"
          :disabled="locked || !capabilities?.available"
          @click="create"
        >
          新建触发任务</button
        ><button
          class="button secondary small"
          :disabled="busy"
          @click="action(loadList)"
        >
          刷新任务列表
        </button>
      </div>
      <p class="trigger-help">
        结果保存在 Web 任务结果中心。当前不向 QQ / 微信主动推送。
      </p>
      <button
        v-for="job in jobs"
        :key="job.id"
        class="trigger-job"
        :class="{ active: selected?.id === job.id }"
        :disabled="locked"
        @click="select(job)"
      >
        <strong>{{ job.title }}</strong
        ><small
          >{{ triggerState(job.lifecycle) }} · 草稿 v{{ job.draft_revision }} /
          生效
          {{ job.active_revision ? `v${job.active_revision}` : "无" }}</small
        ><span>{{
          triggerSummary(
            (job.active_definition || job.task_trigger_draft).trigger,
          )
        }}</span
        ><small v-if="job.block_reason" class="trigger-error"
          >阻断：{{ job.block_reason }}</small
        ><small>{{
          job.next_fire_at
            ? `下次：${triggerDate(job.next_fire_at, (job.active_definition || job.task_trigger_draft).time_zone)}`
            : "下次时间：待启用、无后续机会或事件驱动"
        }}</small>
      </button>
      <p v-if="!jobs.length && !busy" class="trigger-help">
        没有触发任务。创建草稿不会自动执行。
      </p>
      <div v-if="total > 20" class="trigger-actions">
        <button
          class="button secondary small"
          :disabled="busy || offset === 0"
          @click="
            offset -= 20;
            action(loadList);
          "
        >
          上一页</button
        ><span
          >{{ offset + 1 }}–{{ Math.min(offset + 20, total) }} /
          {{ total }}</span
        ><button
          class="button secondary small"
          :disabled="busy || offset + 20 >= total"
          @click="
            offset += 20;
            action(loadList);
          "
        >
          下一页
        </button>
      </div>
      <details class="trigger-calendar">
        <summary>中国日历覆盖</summary>
        <p
          v-for="(calendar, index) in capabilities?.calendars || []"
          :key="index"
        >
          {{ calendar.calendar_id || calendar.id || "cn-official" }} · v{{
            calendar.revision
          }}<br />{{
            calendar.coverage_start || calendar.document?.year || calendar.year
          }}
          —
          {{
            calendar.coverage_end || calendar.document?.year || calendar.year
          }}
          · {{ calendar.active === false ? "草稿未应用" : "已应用" }}
        </p>
        <p class="trigger-help">
          工作日含官方补班；休息日含放假调休。未知年份阻断，不按周末近似。
        </p>
      </details>
      <p class="trigger-help">
        执行结果保留
        {{ capabilities?.retention?.result_days ?? 30 }} 天，原始事件资料保留
        {{ capabilities?.retention?.event_body_days ?? 7 }}
        天。清理正文后仍保留执行状态与去重键。
      </p>
    </aside>

    <main class="trigger-detail">
      <p v-if="error" class="trigger-alert" role="alert">{{ error }}</p>
      <p v-if="notice" class="trigger-notice" role="status">{{ notice }}</p>
      <section
        v-if="capabilities?.clock && !capabilities.clock.trusted"
        class="panel trigger-clock"
        aria-label="服务器时钟核对"
      >
        <h3>服务器时钟需要核对</h3>
        <p class="trigger-help">
          {{
            capabilities.clock.reason || "系统未提供可信同步状态"
          }}。参考时间：{{
            triggerDate(capabilities.now, "UTC")
          }}。规则可保存和预演，自动执行暂被阻断。
        </p>
        <button
          class="button secondary small"
          :disabled="locked"
          @click="acknowledgeClock"
        >
          核对并确认服务器时间
        </button>
      </section>
      <section
        v-if="attempt"
        class="panel trigger-uncertain"
        aria-label="核对未确认的任务操作"
      >
        <p>此操作可能已到达服务器。请保留原请求编号核对结果。</p>
        <button
          class="button secondary"
          :disabled="busy"
          @click="perform(attempt!)"
        >
          原样重试上次操作</button
        ><button
          class="button secondary"
          :disabled="busy"
          @click="action(loadList)"
        >
          只刷新服务器列表
        </button>
      </section>
      <section
        v-if="form"
        class="panel trigger-editor"
        aria-label="触发任务详情"
      >
        <div class="panel-heading">
          <div>
            <h2>{{ selected ? "编辑触发任务" : "新建触发任务" }}</h2>
            <p>
              {{ dirty ? "有未保存的修改" : "草稿已保存"
              }}<template v-if="selected">
                · 生效版本
                {{
                  selected.active_revision
                    ? `v${selected.active_revision}`
                    : "无"
                }}</template
              >
            </p>
          </div>
          <span v-if="selected" class="badge neutral">{{
            triggerState(selected.lifecycle)
          }}</span>
        </div>
        <p v-if="selected?.block_reason" class="trigger-alert">
          任务当前阻断：{{ selected.block_reason }}。启用状态不代表已经执行。
        </p>
        <fieldset :disabled="locked || !capabilities?.available">
          <label class="field"
            ><span>任务名称</span
            ><input
              v-model="form.title"
              maxlength="128"
              placeholder="例如：工作日晨间规划"
          /></label>
          <div class="trigger-field-row">
            <label class="field"
              ><span>触发方式</span
              ><select v-model="preset" aria-label="触发方式">
                <option value="once_after">首次启用后 N 秒（一次）</option>
                <option value="once_at">指定日期和时间（一次）</option>
                <option value="interval">每隔 N 秒重复</option>
                <option value="daily">每天指定时间</option>
                <option value="weekly">每周指定星期</option>
                <option value="monthly">每月指定日期 / 月末</option>
                <option value="yearly">每年指定月日</option>
                <option value="dates">指定日期列表</option>
                <option value="workday">中国工作日</option>
                <option value="restday">中国休息日</option>
                <option value="event">已授权事件</option>
              </select></label
            ><label class="field"
              ><span>任务时区</span
              ><input
                v-model="form.time_zone"
                placeholder="Asia/Shanghai"
              /><small
                >IANA 时区。中国工作日 / 休息日使用 Asia/Shanghai。</small
              ></label
            >
          </div>
          <label v-if="preset === 'once_at'" class="field"
            ><span>触发日期与时间（含时区偏移）</span
            ><input
              v-model="onceAt"
              placeholder="2026-11-01T09:00:00+08:00"
            /><small
              >须包含年份、日期、秒与偏移。服务端校验偏移和时区是否一致。</small
            ></label
          >
          <label
            v-if="['once_after', 'interval'].includes(preset)"
            class="field"
            ><span id="trigger-seconds-label">{{
              preset === "interval" ? "重复间隔（秒）" : "首次启用后等待（秒）"
            }}</span
            ><input
              v-model.number="seconds"
              aria-labelledby="trigger-seconds-label"
              aria-describedby="trigger-seconds-help"
              type="number"
              min="1"
              max="31536000"
            /><small id="trigger-seconds-help">{{
              preset === "interval"
                ? "起点固定，重启不重置。模型重复任务最小 60 秒。"
                : "从首次显式启用事务起算；保存草稿不开始倒计时。"
            }}</small></label
          >
          <template
            v-if="
              !['once_at', 'once_after', 'interval', 'event'].includes(preset)
            "
          >
            <label v-if="!['workday', 'restday'].includes(preset)" class="field"
              ><span>日历日期筛选</span
              ><select v-model="calendarDayClass">
                <option value="any">不叠加工作日筛选</option>
                <option value="workday">同时满足中国工作日</option>
                <option value="restday">同时满足中国休息日</option></select
              ><small>与下方星期或日期选择取交集，不自动顺延。</small></label
            >
            <label class="field"
              ><span>每天的触发时刻</span
              ><input
                v-model="atTimes"
                placeholder="09:00:00, 18:00:00"
              /><small
                >HH:mm:ss，逗号分隔，最多 24 个；相同机会不重复。</small
              ></label
            >
            <label v-if="preset === 'weekly'" class="field"
              ><span>星期列表</span
              ><input v-model="weekdays" placeholder="1,2,3,4,5" /><small
                >1=周一，7=周日；固定星期不扣除法定假日。</small
              ></label
            >
            <label v-if="preset === 'monthly'" class="field"
              ><span>每月日期</span
              ><input v-model="monthDays" placeholder="1,15,last" /><small
                >last 表示月末；每月 31 日遇短月会跳过。</small
              ></label
            >
            <label v-if="preset === 'yearly'" class="field"
              ><span>每年月日</span
              ><input v-model="monthDates" placeholder="02-29, 10-01" /><small
                >MM-DD，逗号分隔；2 月 29 日在非闰年跳过。</small
              ></label
            >
            <label v-if="preset === 'dates'" class="field"
              ><span>具体日期</span
              ><textarea
                v-model="dateList"
                rows="3"
                placeholder="2026-11-01&#10;2026-11-15"
              ></textarea
              ><small>YYYY-MM-DD，每行或逗号分隔，最多 100 个。</small></label
            >
          </template>
          <template v-if="preset === 'event'">
            <label class="field"
              ><span>事件来源与类型</span
              ><select v-model="eventChoice">
                <option value="">选择已授权事件</option>
                <option
                  v-for="event in capabilities?.event_types || []"
                  :key="event.source_id + event.event_type"
                  :value="`${event.source_id}|${event.event_type}`"
                >
                  {{ event.event_type }} · {{ event.source_id }}
                </option></select
              ><small
                >仅服务器可信事件；用户正文不能宣称自己是一条系统事件。</small
              ></label
            >
            <p v-if="activeEvent" class="trigger-help">
              允许筛选字段：{{
                activeEvent.fields
                  .map((item) =>
                    typeof item === "string" ? item : JSON.stringify(item),
                  )
                  .join("、") || "无"
              }}
            </p>
            <label class="field"
              ><span>事件筛选条件 JSON</span
              ><textarea
                v-model="filters"
                rows="4"
                spellcheck="false"
              ></textarea
              ><small
                >最多 8 条 eq / neq / in，条件间为
                AND；不运行代码、正则或任意路径。</small
              ></label
            >
            <div class="trigger-field-row">
              <label class="field"
                ><span>事件冷却（秒）</span
                ><input
                  v-model.number="eventCooldown"
                  type="number"
                  min="0"
                  max="86400"
                /><small>冷却内抑制事件，不在窗口末尾补发。</small></label
              ><label class="field"
                ><span>事件日期筛选</span
                ><select v-model="eventDayClass">
                  <option value="any">任意日期</option>
                  <option value="workday">中国工作日</option>
                  <option value="restday">中国休息日</option>
                </select></label
              >
            </div>
          </template>
          <hr />
          <h3>执行内容</h3>
          <label class="field"
            ><span>任务动作</span
            ><select v-model="form.action.type" @change="changeAction">
              <option value="notify">固定提醒（不调用模型）</option>
              <option value="assistant_run">助手执行（调用模型）</option>
            </select></label
          >
          <label class="field"
            ><span id="trigger-action-label">{{
              isModel ? "助手执行要求" : "固定提醒内容"
            }}</span
            ><textarea
              v-model="actionText"
              aria-labelledby="trigger-action-label"
              aria-describedby="trigger-action-help"
              rows="5"
              maxlength="16000"
              :placeholder="
                isModel ? '明确说明每次执行需要完成什么…' : '填写提醒内容…'
              "
            ></textarea
            ><small id="trigger-action-help"
              >最多 16,000 UTF-8 字节。不提供 shell 或任意系统文件操作。</small
            ></label
          >
          <template v-if="isModel">
            <div class="trigger-field-row">
              <label class="field"
                ><span>任务 Skill</span
                ><select v-model="skillChoice">
                  <option value="">不选择 Skill</option>
                  <option
                    v-for="skill in capabilities?.skills || []"
                    :key="skill.id"
                    :value="skill.id"
                  >
                    {{ skill.name }} · v{{ skill.revision }}
                  </option>
                </select></label
              >
            </div>
            <label class="field"
              ><span>批准的输出方式</span
              ><select v-model="outputMode">
                <option value="text">文本</option>
                <option value="html_drawing">
                  HTML 绘画（需要绘画 Skill）
                </option>
                <option value="native_image">
                  原生 PNG（需要绘画 Skill 与模型支持）
                </option></select
              ><small
                >启用后不会因模型开关改变而自动升级执行方式。每次独立上下文，不读取普通对话或长期记忆。</small
              ></label
            >
          </template>
          <p class="trigger-help">
            结果去向：Web 任务结果中心。保存草稿不发送通知，不创建模型请求。
          </p>
          <details class="trigger-advanced">
            <summary>生效窗口、漏触发与预算</summary>
            <div class="trigger-field-row">
              <label class="field"
                ><span>生效起点（含边界，可空）</span
                ><input
                  v-model="form.active_window.start_at"
                  placeholder="2026-11-01T00:00:00+08:00" /></label
              ><label class="field"
                ><span>结束时间（不含边界，可空）</span
                ><input
                  v-model="form.active_window.end_at"
                  placeholder="2026-12-01T00:00:00+08:00"
              /></label>
            </div>
            <div class="trigger-field-row">
              <label class="field"
                ><span>错过触发的处理</span
                ><select v-model="form.policy.misfire">
                  <option value="skip">跳过过期机会</option>
                  <option value="coalesce_one">有限窗口补最近一次</option>
                  <option v-if="!isModel" value="bounded_catch_up">
                    有限窗口补最多 3 次提醒
                  </option>
                </select></label
              ><label class="field"
                ><span>允许入队迟到（秒）</span
                ><input
                  v-model.number="form.policy.grace_seconds"
                  type="number"
                  min="1"
                  max="86400"
              /></label>
            </div>
            <div
              v-if="form.policy.misfire !== 'skip'"
              class="trigger-field-row"
            >
              <label class="field"
                ><span>最大补跑迟到（秒）</span
                ><input
                  v-model.number="form.policy.catch_up_window_seconds"
                  type="number"
                  min="60"
                  max="86400"
                /><small>须大于允许入队迟到；最多 24 小时。</small></label
              ><label
                v-if="form.policy.misfire === 'bounded_catch_up'"
                class="field"
                ><span>最多补跑次数</span
                ><input
                  v-model.number="form.policy.max_catch_up"
                  type="number"
                  min="1"
                  max="3"
              /></label>
            </div>
            <label class="field"
              ><span>前一次未完成时</span
              ><select v-model="form.policy.overlap">
                <option value="coalesce_one">短时合并最近一次</option>
                <option value="skip">跳过新机会</option></select
              ><small>合并沿用原机会截止时间，不无限延长。</small></label
            >
            <div class="trigger-field-row">
              <label class="field"
                ><span>每日受理机会数</span
                ><input
                  v-model.number="form.budget.daily_occurrences"
                  type="number"
                  min="1"
                  max="10000" /></label
              ><label v-if="isModel" class="field"
                ><span>每日模型调用上限</span
                ><input
                  v-model.number="form.budget.daily_model_runs"
                  type="number"
                  min="1"
                  max="100"
              /></label>
            </div>
            <div v-if="isModel" class="trigger-field-row">
              <label class="field"
                ><span>单次输出 token 上限</span
                ><input
                  v-model.number="form.budget.max_output_tokens"
                  type="number"
                  min="128"
                  max="4096" /></label
              ><label class="field"
                ><span>单次超时（秒）</span
                ><input
                  v-model.number="form.budget.run_timeout_seconds"
                  type="number"
                  min="5"
                  max="300"
              /></label>
            </div>
          </details>
        </fieldset>
        <div class="trigger-actions">
          <button
            class="button primary"
            :disabled="locked || !dirty || !capabilities?.available"
            @click="save"
          >
            保存停用草稿</button
          ><button
            class="button secondary"
            :disabled="locked || !capabilities?.available"
            @click="previewRule"
          >
            校验并预演规则</button
          ><button
            class="button primary"
            :disabled="!canActivate"
            @click="manage('activate')"
          >
            应用并启用
          </button>
        </div>
        <p class="trigger-help">
          修改生效任务先保存草稿，再预演和应用。已运行的执行保留原快照；下一次由服务器核算。
        </p>
        <section
          v-if="preview"
          class="trigger-preview"
          aria-label="触发规则预演"
        >
          <h3>{{ preview.valid ? "规则校验通过" : "请修正规则" }}</h3>
          <p>
            服务器参考时间：{{ triggerDate(preview.now, "UTC")
            }}<template v-if="preview.estimated">
              · 以下按现在启用估算，实际锚点在启用时固定</template
            >
          </p>
          <p class="trigger-help">任务时区：{{ form.time_zone }}</p>
          <ul v-if="preview.errors?.length" class="trigger-error">
            <li v-for="(issue, index) in preview.errors" :key="index">
              {{ issue.field }}：{{ issue.message }}
            </li>
          </ul>
          <p v-if="preview.blocked_reason" class="trigger-error">
            阻断原因：{{ preview.blocked_reason }}
          </p>
          <ol v-if="preview.next_times?.length">
            <li v-for="time in preview.next_times" :key="time.key">
              <strong>{{ time.local }}</strong
              ><small>{{ new Date(time.at * 1000).toISOString() }}</small>
            </li>
          </ol>
          <p v-else class="trigger-help">
            暂无可列出的时间。事件规则等待未来匹配；缺少日历或依赖时不会编造预计触发。
          </p>
          <p v-if="dirty" class="trigger-help">
            这是未保存草稿的预演。请保存后重新预演，才能启用。
          </p>
        </section>
        <template v-if="selected">
          <hr />
          <h3>任务控制</h3>
          <div class="trigger-actions">
            <button
              class="button secondary small"
              :disabled="!eligible || selected.lifecycle !== 'enabled'"
              @click="manage('pause')"
            >
              暂停新触发</button
            ><button
              class="button secondary small"
              :disabled="!eligible || selected.lifecycle !== 'paused'"
              @click="manage('resume')"
            >
              恢复未来触发</button
            ><button
              class="button secondary small"
              :disabled="!eligible || selected.lifecycle === 'archived'"
              @click="confirmAction = 'stop'"
            >
              停止并取消当前</button
            ><button
              class="button secondary small"
              :disabled="!eligible || selected.lifecycle === 'archived'"
              @click="confirmAction = 'test-runs'"
            >
              立即执行一次</button
            ><button
              class="button secondary small"
              :disabled="!eligible || selected.lifecycle === 'archived'"
              @click="confirmAction = 'archive'"
            >
              归档任务
            </button>
          </div>
          <div v-if="confirmAction" class="trigger-confirm" role="alert">
            <p>
              {{
                confirmAction === "test-runs"
                  ? "这会真实执行已保存的任务，模型动作会消耗预算；不会推进正式计划。"
                  : confirmAction === "stop"
                    ? "停止未来触发并取消当前执行；已经发出的网络请求需结算后才释放资源。"
                    : "归档后停止未来触发，历史执行和结果按保留策略保存。"
              }}
            </p>
            <button
              class="button secondary small"
              :disabled="locked"
              @click="manage(confirmAction!)"
            >
              确认{{
                confirmAction === "test-runs"
                  ? "立即执行"
                  : confirmAction === "stop"
                    ? "停止"
                    : "归档"
              }}</button
            ><button
              class="button secondary small"
              @click="confirmAction = null"
            >
              取消操作
            </button>
          </div>
          <p class="trigger-help">
            任务编号 {{ selected.id }} · 编辑版本 {{ selected.revision
            }}<br />{{
              selected.next_fire_at
                ? `服务器下次时间：${triggerDate(selected.next_fire_at, (selected.active_definition || selected.task_trigger_draft).time_zone)}`
                : "服务器尚无下次时间或按事件驱动。"
            }}
          </p>
        </template>
      </section>
      <section v-else class="panel trigger-empty">
        <h2>把未来安排交给明确的规则</h2>
        <p>
          选择左侧任务查看详情，或新建日期、间隔、工作日、休息日及事件任务。
        </p>
        <p>也可以在对话中使用「设置触发任务」Skill，再点击“审核任务草稿”。</p>
      </section>
      <section
        v-if="selected"
        class="panel trigger-history"
        aria-label="任务执行记录"
      >
        <div class="panel-heading">
          <div>
            <h2>执行记录与结果</h2>
            <p>执行完成、结果保存和投递状态分别记录。</p>
          </div>
          <button
            class="button secondary small"
            :disabled="busy"
            @click="action(history)"
          >
            刷新执行记录
          </button>
        </div>
        <p v-if="!occurrences.length" class="trigger-help">
          尚无执行记录。预演不会产生记录。
        </p>
        <button
          v-for="item in occurrences"
          :key="item.id"
          class="trigger-occurrence"
          :disabled="busy"
          @click="inspect(item)"
        >
          <span
            ><strong>{{ triggerState(item.status) }}</strong> ·
            {{ item.origin === "manual_test" ? "手动执行" : "计划触发" }}</span
          ><small
            >计划
            {{
              triggerDate(
                item.nominal_at,
                item.time_zone || selected.task_trigger_draft.time_zone,
              )
            }}
            · {{ triggerState(item.delivery_status) }}</small
          >
        </button>
        <div v-if="occurrenceTotal > 20" class="trigger-actions">
          <button
            class="button secondary small"
            :disabled="busy || occurrenceOffset === 0"
            @click="
              occurrenceOffset -= 20;
              action(history);
            "
          >
            更早页前</button
          ><span
            >{{ occurrenceOffset + 1 }}–{{
              Math.min(occurrenceOffset + 20, occurrenceTotal)
            }}
            / {{ occurrenceTotal }}</span
          ><button
            class="button secondary small"
            :disabled="busy || occurrenceOffset + 20 >= occurrenceTotal"
            @click="
              occurrenceOffset += 20;
              action(history);
            "
          >
            更早记录
          </button>
        </div>
        <article
          v-if="inspected"
          class="trigger-result"
          aria-label="触发任务结果"
        >
          <h3>{{ triggerState(inspected.status) }}</h3>
          <p class="trigger-help">
            任务定义 v{{ inspected.job_revision }} · 计划
            {{
              triggerDate(
                inspected.nominal_at,
                inspected.time_zone || selected.task_trigger_draft.time_zone,
              )
            }}<br />开始
            {{
              triggerDate(
                inspected.started_at,
                inspected.time_zone || selected.task_trigger_draft.time_zone,
              )
            }}
            · 完成
            {{
              triggerDate(
                inspected.completed_at,
                inspected.time_zone || selected.task_trigger_draft.time_zone,
              )
            }}<br />结果状态：{{ triggerState(inspected.delivery_status) }}
          </p>
          <p v-if="inspected.error" class="trigger-error">
            {{ inspected.error }}
          </p>
          <SkillOutput
            v-if="inspected.response || inspected.generated_image_url"
            :content="inspected.response || ''"
            :generated-image-url="inspected.generated_image_url"
            :reviewable="false"
          />
          <p v-else class="trigger-help">暂无已保存的结果。</p>
          <button
            v-if="
              inspected.delivery_status === 'failed' &&
              !inspected.result_expired
            "
            class="button secondary small"
            :disabled="locked"
            @click="retryDelivery"
          >
            仅重试保存 / 投递既有结果
          </button>
        </article>
      </section>
      <section
        class="panel trigger-calendar-management"
        aria-label="年度日历管理"
      >
        <details>
          <summary>管理中国年度日历</summary>
          <p class="trigger-help">
            仅导入你已核对的官方放假 / 调休安排。导入先保存草稿，不抓取任意
            URL；应用后影响全实例的未来工作日 / 休息日判断。
          </p>
          <button
            class="button secondary small"
            :disabled="busy"
            @click="action(loadCalendars)"
          >
            读取日历版本
          </button>
          <button
            v-for="calendar in calendars"
            :key="calendar.id"
            class="trigger-help"
            :disabled="locked"
            @click="inspectCalendar(calendar)"
          >
            {{ calendar.document.year }} 年 · v{{ calendar.revision }} ·
            {{ calendar.active ? "已应用" : "未应用草稿" }} ·
            {{ calendar.document.source.document_number }}
          </button>
          <label class="field"
            ><span>官方年度日历 JSON</span
            ><textarea
              v-model="calendarInput"
              rows="8"
              :disabled="locked"
              placeholder='{"year":2027,"time_zone":"Asia/Shanghai","holidays":[],"working_days":[],"source":{"authority":"发布机关","document_number":"官方文号","published_at":"YYYY-MM-DD","url":"官方来源 HTTPS 链接"}}'
              spellcheck="false"
            ></textarea></label
          ><button
            class="button secondary small"
            :disabled="locked || !calendarInput.trim()"
            @click="saveCalendarDraft"
          >
            保存日历导入草稿
          </button>
          <div v-if="calendarDraft && calendarChanges" class="trigger-preview">
            <h3>{{ calendarDraft.document.year }} 年日历差异</h3>
            <p>
              此前：{{
                calendarChanges.previous
                  ? `v${calendarChanges.previous}`
                  : "无已应用版本"
              }}
              → 待应用 v{{ calendarDraft.revision }}
            </p>
            <p>
              新增放假：{{ calendarChanges.holidays.added.join("、") || "无"
              }}<br />移除放假：{{
                calendarChanges.holidays.removed.join("、") || "无"
              }}<br />新增补班：{{
                calendarChanges.working.added.join("、") || "无"
              }}<br />移除补班：{{
                calendarChanges.working.removed.join("、") || "无"
              }}
            </p>
            <p>
              来源：{{ calendarDraft.document.source.authority }} ·
              {{ calendarDraft.document.source.document_number }} ·
              {{ calendarDraft.document.source.published_at }}<br />{{
                calendarDraft.document.source.url
              }}
            </p>
            <p>
              本人正在启用且依赖中国日历的任务：{{ calendarAffected.length }}
              条；其他管理员任务可能同样受影响，不展示其私有内容。
            </p>
            <label class="trigger-verify"
              ><input
                v-model="calendarVerified"
                type="checkbox"
                :disabled="locked"
              />我已核对官方来源、放假及补班日期</label
            ><button
              class="button primary small"
              :disabled="locked || !calendarVerified || !calendarImpactReady"
              @click="applyCalendar"
            >
              核验并应用日历版本
            </button>
          </div>
        </details>
      </section>
    </main>
  </div>
</template>

<style scoped>
.trigger-calendar-management summary {
  cursor: pointer;
  font-weight: 600;
}
.trigger-calendar-management textarea {
  width: 100%;
  box-sizing: border-box;
  border: 1px solid var(--line, #dce2f3);
  border-radius: 10px;
  padding: 12px;
  resize: vertical;
  font: inherit;
}
.trigger-verify {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  margin: 14px 0;
  font-size: 12px;
}
.task-trigger-workspace {
  display: grid;
  grid-template-columns: 280px minmax(0, 1fr);
  gap: 22px;
  align-items: start;
}
.trigger-library,
.trigger-detail,
.trigger-editor,
.trigger-history {
  min-width: 0;
}
.trigger-detail {
  display: flex;
  flex-direction: column;
  gap: 20px;
}
.trigger-detail > .panel {
  margin: 0;
}
.trigger-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 9px;
  align-items: center;
  margin: 16px 0;
}
.trigger-actions .button {
  max-width: 100%;
  white-space: normal;
}
.trigger-job,
.trigger-occurrence {
  display: block;
  width: 100%;
  text-align: left;
  border: 1px solid var(--line, #dce2f3);
  border-radius: 12px;
  background: #fff;
  padding: 14px;
  color: inherit;
  margin: 10px 0;
  cursor: pointer;
  overflow-wrap: anywhere;
}
.trigger-job.active {
  background: #eef0fa;
  border-color: #4b5d97;
}
.trigger-job strong,
.trigger-job small,
.trigger-job > span,
.trigger-occurrence small {
  display: block;
}
.trigger-job small,
.trigger-occurrence small {
  font-size: 11px;
  color: var(--muted, #68779b);
  line-height: 1.8;
  margin-top: 5px;
}
.trigger-job > span {
  font-size: 12px;
  margin-top: 8px;
}
.trigger-help {
  font-size: 12px;
  line-height: 1.8;
  color: var(--muted, #68779b);
  overflow-wrap: anywhere;
}
.trigger-alert,
.trigger-notice {
  padding: 14px 18px;
  border-radius: 12px;
  font-size: 13px;
  line-height: 1.8;
  margin: 0;
  overflow-wrap: anywhere;
}
.trigger-alert {
  background: #fff1ee;
  color: #973f38;
  border: 1px solid #edccc5;
}
.trigger-notice {
  background: #edf6f1;
  color: #3b6755;
  border: 1px solid #d1e9dc;
}
.trigger-error {
  color: #a03e38;
}
.trigger-field-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  gap: 16px;
}
.trigger-editor fieldset {
  border: 0;
  padding: 0;
  margin: 0;
  min-width: 0;
}
.trigger-editor .field {
  min-width: 0;
}
.trigger-editor select,
.trigger-editor input,
.trigger-editor textarea {
  width: 100%;
  min-width: 0;
  max-width: 100%;
  border: 1px solid var(--line, #dce2f3);
  border-radius: 10px;
  padding: 12px;
  background: white;
  color: inherit;
  font: inherit;
  box-sizing: border-box;
}
.trigger-editor select:disabled {
  opacity: 0.6;
}
.trigger-editor textarea {
  resize: vertical;
}
.trigger-editor hr {
  border: 0;
  border-top: 1px solid var(--line, #dce2f3);
  margin: 24px 0;
}
.trigger-editor h3,
.trigger-history h3 {
  font-size: 15px;
  margin: 18px 0;
}
.trigger-advanced,
.trigger-calendar {
  border-top: 1px solid var(--line, #dce2f3);
  padding-top: 16px;
  margin-top: 18px;
  font-size: 12px;
}
.trigger-advanced summary,
.trigger-calendar summary {
  cursor: pointer;
  font-weight: 600;
  margin-bottom: 15px;
}
.trigger-preview {
  padding: 18px;
  background: #f5f7fc;
  border: 1px solid var(--line, #dce2f3);
  border-radius: 12px;
  font-size: 12px;
  line-height: 1.8;
  overflow-wrap: anywhere;
}
.trigger-preview ol {
  padding-left: 22px;
}
.trigger-preview li {
  margin: 8px 0;
}
.trigger-preview small {
  display: block;
  color: var(--muted, #68779b);
}
.trigger-confirm {
  padding: 14px;
  background: #fff5ed;
  border-radius: 12px;
  font-size: 12px;
  line-height: 1.8;
}
.trigger-confirm button {
  margin-right: 8px;
}
.trigger-empty {
  line-height: 1.8;
  padding-top: 36px;
  padding-bottom: 36px;
}
.trigger-result {
  background: #f6f7fc;
  border: 1px solid var(--line, #dce2f3);
  border-radius: 12px;
  padding: 18px;
  margin-top: 18px;
  overflow-wrap: anywhere;
}
.trigger-result :deep(.message-content) {
  white-space: pre-wrap;
}
.trigger-uncertain {
  font-size: 12px;
  line-height: 1.8;
}
.trigger-uncertain button {
  margin: 8px 8px 0 0;
}
@media (max-width: 1100px) {
  .task-trigger-workspace {
    grid-template-columns: 235px minmax(0, 1fr);
  }
}
@media (max-width: 850px) {
  .task-trigger-workspace {
    grid-template-columns: minmax(0, 1fr);
  }
  .trigger-library {
    max-height: none;
  }
  .trigger-field-row {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
