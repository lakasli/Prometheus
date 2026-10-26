<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref } from "vue";
import { api, ApiError, errorMessage } from "../api";
import {
  activityLabels,
  calendarRange,
  dateKey,
  displayTime,
  label,
  localInput,
  localTimestamp,
  shiftDay,
} from "../life-systems";
import { blockReason, planningStatus, shiftMonth } from "../calendar-extras";
import type { CalendarView } from "../life-systems";
import type { CalendarRevision } from "../task-triggers";
interface Activity {
  title: string;
  type: string;
  purpose: string;
  start_at: number;
  end_at: number;
  time_zone: string;
  resource_id: string | null;
  target_ref: string | null;
  repeat: {
    frequency: string;
    interval: number;
    until: number | null;
    count: number | null;
  } | null;
  movable: boolean;
  restart_reading?: boolean;
  grant_id: string | null;
}
interface Occurrence {
  id: string;
  nominal_at: number;
  start_at: number;
  end_at: number | null;
  status: string;
  block_reason: string | null;
  processed_chars: number;
  total_chars: number | null;
  result?: unknown;
  started_at?: number | null;
  completed_at?: number | null;
}
interface Detail {
  id: string;
  revision: number;
  state: string;
  creator: string;
  definition: Activity;
  occurrences: Occurrence[];
  exceptions: unknown[];
}
interface CalendarItem {
  id: string;
  activity_id: string;
  revision: number;
  nominal_at: number;
  title: string;
  type: string;
  state: string;
  status: string;
  start_at: number;
  end_at: number | null;
  origin: string;
  movable?: boolean;
  block_reason: string | null;
  progress?: Occurrence | null;
  delivery_status?: string;
}
interface Resource {
  id: string;
  title: string;
  revision: number;
  characters: number;
  revoked_at: number | null;
  progress?: {
    generation: number;
    processed_chars: number;
    total_chars: number;
  } | null;
}
interface Grant {
  id: string;
  revision: number;
  revoked_at: number | null;
  policy: {
    title: string;
    allowed_types: string[];
    resource_ids: string[];
    valid_until: number;
    max_daily_activities: number;
    max_daily_minutes: number;
    window_start: string;
    window_end: string;
  };
}
interface Plan {
  id: string;
  revision: number;
  state: string;
  definition: {
    goal: string;
    grant_id: string;
    resource_ids: string[];
    duration_minutes: number;
    state: string;
  };
  last_planned_at: number | null;
  planning_diagnostics?: {
    status: string;
    last_attempt: {
      status: string;
      error_code: string | null;
      finished_at: number | null;
    } | null;
  };
}
const emit = defineEmits<{ unauthorized: []; openDiary: [] }>();
const tab = ref("calendar"),
  view = ref<CalendarView>(
    window.matchMedia("(max-width: 700px)").matches ? "agenda" : "week",
  );
const timeZone = ref("Asia/Shanghai"),
  anchor = ref(dateKey(Date.now() / 1000));
const items = ref<CalendarItem[]>([]),
  legacyEvents = ref<{ id: string; title: string }[]>([]);
const resources = ref<Resource[]>([]),
  grants = ref<Grant[]>([]),
  plans = ref<Plan[]>([]);
const calendars = ref<CalendarRevision[]>([]);
const calendarElement = ref<HTMLElement | null>(null);
let focusKey = "";
const busy = ref(false),
  error = ref(""),
  notice = ref(""),
  total = ref(0),
  offset = ref(0);
const capabilities = ref<{
  clock?: { trusted: boolean };
  model_planning?: boolean;
}>({});
const filters = reactive({ type: "", creator: "", state: "" });
const editor = ref(false),
  selected = ref<Detail | null>(null),
  nominalAt = ref<number | null>(null);
const editScope = ref("series"),
  preview = ref<{
    conflicts: unknown[];
    occurrences: unknown[];
    capability_block: string | null;
  } | null>(null);
const mutationKey = ref(crypto.randomUUID());
const approvalConfirmed = ref(false);
const form = reactive({
  title: "",
  type: "reading",
  purpose: "",
  start: "",
  end: "",
  resource: "",
  grant: "",
  target: "",
  frequency: "",
  repeatCount: 7,
  movable: true,
  restart: false,
});
const resourceForm = reactive({ title: "", content: "", allow: false });
const grantForm = reactive({
  title: "",
  resourceIds: [] as string[],
  start: "09:00",
  end: "18:00",
  days: 30,
  dailyMinutes: 60,
  dailyCount: 3,
  modelCalls: 8,
  modelTokens: 24000,
});
const planForm = reactive({
  goal: "",
  grant: "",
  resourceIds: [] as string[],
  minutes: 20,
  planningMode: "bounded_rule",
});
let disposed = false,
  timer: ReturnType<typeof setTimeout> | undefined;
const controller = new AbortController();
const safeZone = computed(() => {
  try {
    new Intl.DateTimeFormat("zh-CN", { timeZone: timeZone.value }).format();
    return timeZone.value;
  } catch {
    return "Asia/Shanghai";
  }
});
const range = computed(() =>
  calendarRange(
    /^\d{4}-\d{2}-\d{2}$/.test(anchor.value)
      ? anchor.value
      : dateKey(Date.now() / 1000),
    view.value,
    safeZone.value,
  ),
);
const activeResources = computed(() =>
  resources.value.filter((r) => !r.revoked_at),
);
const activeGrants = computed(() => grants.value.filter((g) => !g.revoked_at));
const dirty = computed(() => editor.value && !!form.title.trim());
defineExpose({
  hasUnsaved: computed(
    () =>
      dirty.value ||
      !!resourceForm.title.trim() ||
      !!resourceForm.content.trim(),
  ),
});
function caught(e: unknown) {
  if (e instanceof ApiError && e.status === 401) emit("unauthorized");
  return e instanceof ApiError
    ? errorMessage(e)
    : e instanceof Error
      ? e.message
      : errorMessage(e);
}
async function task(action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  notice.value = "";
  try {
    await action();
  } catch (e) {
    if (!disposed) error.value = caught(e);
  } finally {
    if (!disposed) busy.value = false;
  }
}
async function load() {
  if (timeZone.value !== safeZone.value)
    throw new Error("请输入有效的 IANA 时区，例如 Asia/Shanghai。");
  const params = new URLSearchParams({
    start: String(range.value.start),
    end: String(range.value.end),
    time_zone: timeZone.value,
    limit: "500",
    offset: String(offset.value),
  });
  for (const [key, value] of Object.entries(filters))
    if (value) params.set(key, value);
  const result = await api<{
    items: CalendarItem[];
    legacy_events: typeof legacyEvents.value;
    total: number;
  }>(`/schedule/calendar?${params}`, { signal: controller.signal });
  if (!disposed) {
    items.value = result.items;
    legacyEvents.value = result.legacy_events;
    total.value = result.total;
    const key = `${view.value}/${range.value.start}/${safeZone.value}`;
    if (view.value !== "month" && view.value !== "agenda" && key !== focusKey) {
      focusKey = key;
      await nextTick();
      const current = dateKey(Date.now() / 1000, safeZone.value);
      const hour = range.value.days.includes(current)
        ? Number(localInput(Date.now() / 1000, safeZone.value).slice(11, 13))
        : 9;
      if (calendarElement.value)
        calendarElement.value.scrollTop = Math.max(0, hour * 36 - 72);
    }
  }
}
async function loadSettings() {
  const [r, g, p, c, calendar] = await Promise.all([
    api<{ items: Resource[] }>("/schedule/resources", {
      signal: controller.signal,
    }),
    api<{ items: Grant[] }>("/schedule/autonomy-grants", {
      signal: controller.signal,
    }),
    api<{ items: Plan[] }>("/schedule/plans", { signal: controller.signal }),
    api<typeof capabilities.value>("/schedule/capabilities", {
      signal: controller.signal,
    }),
    api<{ items: CalendarRevision[] }>("/task-triggers/calendars", {
      signal: controller.signal,
    }),
  ]);
  if (!disposed) {
    resources.value = r.items;
    grants.value = g.items;
    plans.value = p.items;
    capabilities.value = c;
    calendars.value = calendar.items;
  }
}
async function refresh() {
  await Promise.all([load(), loadSettings()]);
}
function eventsFor(day: string) {
  return items.value.filter(
    (item) => dateKey(item.start_at, safeZone.value) === day,
  );
}
function dayClass(day: string) {
  const calendar = calendars.value.find(
    (c) =>
      c.active &&
      c.verified_by &&
      c.document.year === Number(day.slice(0, 4)) &&
      c.document.time_zone === safeZone.value,
  );
  if (!calendar) return "日历未核验";
  if (calendar.document.working_days.includes(day)) return "工作";
  if (calendar.document.holidays.includes(day)) return "休息";
  return [0, 6].includes(new Date(`${day}T12:00Z`).getUTCDay())
    ? "休息"
    : "工作";
}
function navigate(amount: number) {
  offset.value = 0;
  anchor.value =
    view.value === "month"
      ? shiftMonth(anchor.value, amount)
      : shiftDay(anchor.value, amount * (view.value === "day" ? 1 : 7));
  void task(load);
}
function eventStyle(item: CalendarItem) {
  if (view.value !== "week" && view.value !== "day") return undefined;
  const clock = localInput(item.start_at, safeZone.value)
    .slice(11)
    .split(":")
    .map(Number);
  const minutes = clock[0]! * 60 + clock[1]!;
  const duration = item.end_at
    ? Math.max(1, (item.end_at - item.start_at) / 60)
    : 15;
  return {
    top: `${44 + minutes * 0.6}px`,
    minHeight: `${Math.max(46, duration * 0.6)}px`,
  };
}
async function newActivity(day = anchor.value) {
  selected.value = null;
  nominalAt.value = null;
  editScope.value = "series";
  preview.value = null;
  Object.assign(form, {
    title: "",
    type: "reading",
    purpose: "",
    start: `${day}T10:00`,
    end: `${day}T10:20`,
    resource: "",
    grant: "",
    target: "",
    frequency: "",
    repeatCount: 7,
    movable: true,
    restart: false,
  });
  mutationKey.value = crypto.randomUUID();
  editor.value = true;
  await nextTick();
  document
    .getElementById("schedule-editor")
    ?.scrollIntoView({ behavior: "smooth", block: "start" });
}
function definition(): Activity {
  return {
    title: form.title,
    type: form.type,
    purpose: form.purpose,
    start_at: localTimestamp(form.start, timeZone.value),
    end_at: localTimestamp(form.end, timeZone.value),
    time_zone: timeZone.value,
    resource_id: form.resource || null,
    target_ref: form.target || null,
    grant_id: form.grant || null,
    movable: form.movable,
    restart_reading: form.restart,
    repeat: form.frequency
      ? {
          frequency: form.frequency,
          interval: 1,
          count: form.repeatCount,
          until: null,
        }
      : null,
  };
}
async function inspect(item: CalendarItem) {
  if (item.origin === "diary_system") {
    emit("openDiary");
    return;
  }
  if (item.origin === "legacy_trigger") {
    notice.value = "这是已有触发任务的只读执行记录。";
    return;
  }
  await task(async () => {
    approvalConfirmed.value = false;
    const detail = await api<Detail>(
      `/schedule/activities/${item.activity_id}`,
    );
    selected.value = detail;
    nominalAt.value = item.nominal_at;
    editScope.value = detail.definition.repeat ? "single" : "series";
    preview.value = null;
    const a = detail.definition;
    timeZone.value = a.time_zone;
    Object.assign(form, {
      title: a.title,
      type: a.type,
      purpose: a.purpose,
      start: localInput(item.start_at, a.time_zone),
      end: localInput(item.end_at ?? a.end_at, a.time_zone),
      resource: a.resource_id || "",
      target: a.target_ref || "",
      grant: a.grant_id || "",
      frequency: a.repeat?.frequency || "",
      repeatCount: a.repeat?.count || 7,
      movable: a.movable,
      restart: a.restart_reading || false,
    });
    editor.value = true;
    await nextTick();
    document
      .getElementById("schedule-editor")
      ?.scrollIntoView({ behavior: "smooth", block: "start" });
  });
}
async function save() {
  await task(async () => {
    const body = definition();
    if (selected.value)
      await api(`/schedule/activities/${selected.value.id}`, {
        method: "PATCH",
        revision: selected.value.revision,
        body: {
          scope: editScope.value,
          nominal_at: nominalAt.value,
          definition: body,
        },
      });
    else
      await api("/schedule/activities", {
        method: "POST",
        body,
        idempotencyKey: mutationKey.value,
      });
    editor.value = false;
    selected.value = null;
    await refresh();
    notice.value = "日程已保存；计划时间与实际执行结果分别记录。";
  });
}
async function previewActivity() {
  await task(async () => {
    preview.value = await api(
      `/schedule/activities${selected.value ? `/${selected.value.id}` : ""}/preview`,
      { method: "POST", body: definition() },
    );
  });
}
async function changeState(action: string) {
  if (!selected.value) return;
  const current = selected.value;
  await task(async () => {
    await api(`/schedule/activities/${current.id}/${action}`, {
      method: "POST",
      revision: current.revision,
      body: { scope: editScope.value, nominal_at: nominalAt.value },
    });
    editor.value = false;
    selected.value = null;
    await load();
    notice.value = "状态已更新。";
  });
}
async function approve() {
  const current = selected.value;
  if (!current || !approvalConfirmed.value) return;
  await task(async () => {
    await api(`/schedule/activities/${current.id}/approve`, {
      method: "POST",
      revision: current.revision,
      body: { confirmed: true, scope: "series" },
    });
    editor.value = false;
    selected.value = null;
    approvalConfirmed.value = false;
    await refresh();
    notice.value =
      "已明确批准当前已保存的整个系列；实际执行仍按资料、能力与预算检查。";
  });
}
async function moveDrop(event: DragEvent, day: string) {
  const id = event.dataTransfer?.getData("text/plain"),
    item = items.value.find((i) => i.id === id);
  if (!item || !item.movable || item.origin !== "schedule") return;
  await task(async () => {
    const detail = await api<Detail>(
      `/schedule/activities/${item.activity_id}`,
    );
    if (detail.revision !== item.revision)
      throw new Error("日程已有更新，请刷新后再改期。");
    const old = localInput(item.start_at, timeZone.value),
      start = localTimestamp(`${day}T${old.slice(11)}`, timeZone.value);
    await api(`/schedule/activities/${item.activity_id}`, {
      method: "PATCH",
      revision: item.revision,
      body: {
        scope: detail.definition.repeat ? "single" : "series",
        nominal_at: item.nominal_at,
        definition: {
          ...detail.definition,
          start_at: start,
          end_at:
            start + ((item.end_at ?? detail.definition.end_at) - item.start_at),
        },
      },
    });
    await load();
    notice.value = "已改期；重复活动只移动本次，原有执行记录保留。";
  });
}
function drag(event: DragEvent, item: CalendarItem) {
  event.dataTransfer?.setData("text/plain", item.id);
}
async function uploadResource() {
  await task(async () => {
    await api("/schedule/resources", {
      method: "POST",
      body: {
        title: resourceForm.title,
        type: "text",
        content: resourceForm.content,
      },
    });
    resourceForm.title = "";
    resourceForm.content = "";
    resourceForm.allow = false;
    await loadSettings();
    notice.value = "阅读资料已保存，可用于当前来源的阅读活动。";
  });
}
async function revokeResource(resource: Resource) {
  await task(async () => {
    await api(`/schedule/resources/${resource.id}/revoke`, {
      method: "POST",
      revision: resource.revision,
      body: {},
    });
    await refresh();
    notice.value = "资料授权已撤销，待执行的阅读会被阻断。";
  });
}
async function grant() {
  await task(async () => {
    const now = Math.floor(Date.now() / 1000);
    await api("/schedule/autonomy-grants", {
      method: "POST",
      body: {
        title: grantForm.title,
        allowed_types: ["reading"],
        resource_ids: grantForm.resourceIds,
        time_zone: timeZone.value,
        valid_from: now,
        valid_until: now + grantForm.days * 86400,
        horizon_days: Math.min(grantForm.days, 90),
        window_start: grantForm.start,
        window_end: grantForm.end,
        max_daily_activities: grantForm.dailyCount,
        max_daily_minutes: grantForm.dailyMinutes,
        max_daily_changes: 12,
        max_daily_model_calls: grantForm.modelCalls,
        max_daily_tokens: grantForm.modelTokens,
      },
    });
    grantForm.title = "";
    await loadSettings();
    notice.value = "自主阅读范围已授权，可随时撤销。";
  });
}
async function revokeGrant(g: Grant) {
  await task(async () => {
    await api(`/schedule/autonomy-grants/${g.id}/revoke`, {
      method: "POST",
      revision: g.revision,
      body: {},
    });
    await refresh();
    notice.value = "自主授权已撤销。";
  });
}
async function createPlan() {
  await task(async () => {
    await api("/schedule/plans", {
      method: "POST",
      body: {
        goal: planForm.goal,
        grant_id: planForm.grant,
        resource_ids: planForm.resourceIds,
        duration_minutes: planForm.minutes,
        state: "enabled",
        planning_mode: planForm.planningMode,
      },
    });
    planForm.goal = "";
    await refresh();
    notice.value = "工作目标已保存，将在授权时间与预算内规划。";
  });
}
async function pausePlan(plan: Plan) {
  await task(async () => {
    await api(`/schedule/plans/${plan.id}`, {
      method: "PATCH",
      revision: plan.revision,
      body: { state: plan.state === "paused" ? "enabled" : "paused" },
    });
    await refresh();
    notice.value = "工作目标已更新。";
  });
}
async function acknowledge() {
  await task(async () => {
    await api("/task-triggers/clock/acknowledge", {
      method: "POST",
      body: { observed_now: Math.floor(Date.now() / 1000), confirmed: true },
    });
    await loadSettings();
    notice.value = "时钟已确认，有效期一小时。";
  });
}
function armRefresh() {
  timer = setTimeout(async () => {
    if (!disposed && !busy.value && !editor.value && tab.value === "calendar") {
      try {
        await load();
      } catch {
        /* keep last sample; explicit refresh reports errors */
      }
    }
    if (!disposed) armRefresh();
  }, 10000);
}
onMounted(() => {
  void task(refresh);
  armRefresh();
});
onUnmounted(() => {
  disposed = true;
  controller.abort();
  clearTimeout(timer);
});
</script>

<template>
  <div class="life-system schedule-system">
    <p v-if="error" role="alert" class="error-banner">{{ error }}</p>
    <p v-if="notice" role="status" class="notice-banner">{{ notice }}</p>
    <section class="panel life-intro">
      <div>
        <p class="eyebrow">TIME FOR WHAT MATTERS</p>
        <h2>安排今天，也看见进展</h2>
        <p>安排日常活动与工作目标。计划、真实进度与投递状态分别记录。</p>
      </div>
      <button :disabled="busy" @click="task(refresh)">刷新</button>
    </section>
    <p
      v-if="capabilities.clock && !capabilities.clock.trusted"
      class="notice-banner"
    >
      服务器时钟尚未确认，自动活动暂不派发。当前浏览器时间：{{
        displayTime(Date.now() / 1000, safeZone)
      }}
      <button :disabled="busy" @click="acknowledge">核对并确认时间</button>
    </p>
    <div class="life-tabs" role="tablist" aria-label="日程功能">
      <button
        v-for="t in [
          { id: 'calendar', name: '日历' },
          { id: 'autonomy', name: '工作目标与授权' },
        ]"
        :key="t.id"
        role="tab"
        :aria-selected="tab === t.id"
        :class="{ active: tab === t.id }"
        @click="tab = t.id"
      >
        {{ t.name }}
      </button>
    </div>
    <section v-if="tab === 'calendar'" class="panel calendar-panel">
      <div class="life-toolbar calendar-heading">
        <div class="life-actions">
          <button aria-label="上一时段" :disabled="busy" @click="navigate(-1)">
            ←</button
          ><button
            :disabled="busy"
            @click="
              anchor = dateKey(Date.now() / 1000, safeZone);
              task(load);
            "
          >
            今天</button
          ><button aria-label="下一时段" :disabled="busy" @click="navigate(1)">
            →
          </button>
        </div>
        <strong>{{ range.days[0] }} — {{ range.days.at(-1) }}</strong>
        <div class="life-grow"></div>
        <button class="primary" @click="newActivity()">新建活动</button>
      </div>
      <form
        class="life-toolbar"
        @submit.prevent="
          offset = 0;
          task(load);
        "
      >
        <label
          >视图<select
            v-model="view"
            @change="
              offset = 0;
              task(load);
            "
          >
            <option value="month">月</option>
            <option value="week">周</option>
            <option value="day">日</option>
            <option value="agenda">议程</option>
          </select></label
        ><label>日期<input v-model="anchor" type="date" required /></label
        ><label
          >时区<input
            v-model="timeZone"
            required
            placeholder="Asia/Shanghai" /></label
        ><label
          >类型<select v-model="filters.type">
            <option value="">全部类型</option>
            <option
              v-for="kind in [
                'reading',
                'video',
                'consolidation',
                'chat',
                'action',
              ]"
              :key="kind"
              :value="kind"
            >
              {{ activityLabels[kind] }}
            </option>
          </select></label
        ><button :disabled="busy">更新日历</button>
      </form>
      <div
        v-if="view !== 'agenda'"
        class="schedule-calendar"
        ref="calendarElement"
        :class="`calendar-view-${view}`"
        :aria-busy="busy"
      >
        <div
          v-if="view === 'week' || view === 'day'"
          class="calendar-hours"
          aria-hidden="true"
        >
          <span
            v-for="hour in 24"
            :key="hour"
            :style="{ top: `${44 + (hour - 1) * 36}px` }"
            >{{ String(hour - 1).padStart(2, "0") }}:00</span
          >
        </div>
        <div
          v-for="day in range.days"
          :key="day"
          class="calendar-day"
          :class="{ today: day === dateKey(Date.now() / 1000, safeZone) }"
          @dragover.prevent
          @drop.prevent="moveDrop($event, day)"
        >
          <button
            class="calendar-day-label"
            :aria-label="`${day}新建活动`"
            @click="newActivity(day)"
          >
            <span>{{ day.slice(5) }}</span
            ><small
              >{{
                ["周日", "周一", "周二", "周三", "周四", "周五", "周六"][
                  new Date(`${day}T12:00Z`).getUTCDay()
                ]
              }}
              · {{ dayClass(day) }}</small
            ></button
          ><button
            v-for="item in eventsFor(day)"
            :key="item.id"
            class="calendar-event"
            :class="`event-${item.type}`"
            :style="eventStyle(item)"
            :draggable="item.movable && item.origin === 'schedule'"
            @dragstart="drag($event, item)"
            @click="inspect(item)"
          >
            <span
              >{{ displayTime(item.start_at, safeZone).split(" ").at(-1) }} ·
              {{ activityLabels[item.type] || item.type }}</span
            ><strong>{{ item.title }}</strong
            ><small
              >{{ label(item.status)
              }}{{ item.block_reason ? " · 已阻断" : "" }}</small
            ><small v-if="item.progress?.total_chars"
              >{{ item.progress.processed_chars }} /
              {{ item.progress.total_chars }} 字</small
            >
          </button>
        </div>
      </div>
      <div v-else class="schedule-agenda">
        <div v-for="day in range.days" :key="day" class="agenda-day">
          <h3>{{ day }}</h3>
          <p v-if="!eventsFor(day).length" class="life-meta">暂无计划</p>
          <button
            v-for="item in eventsFor(day)"
            :key="item.id"
            class="agenda-item"
            @click="inspect(item)"
          >
            <span
              >{{ displayTime(item.start_at, safeZone)
              }}<small>{{
                item.end_at ? displayTime(item.end_at, safeZone) : "时长未指定"
              }}</small></span
            ><span
              ><strong>{{ item.title }}</strong
              ><small
                >{{ activityLabels[item.type] || item.type }} ·
                {{ label(item.status) }}</small
              ><small v-if="item.block_reason">{{
                item.block_reason
              }}</small></span
            >
          </button>
        </div>
      </div>
      <div v-if="total > 500" class="life-actions life-pagination">
        <button
          :disabled="offset === 0 || busy"
          @click="
            offset -= 500;
            task(load);
          "
        >
          上一页</button
        ><span
          >{{ offset + 1 }}–{{ Math.min(offset + 500, total) }} /
          {{ total }}</span
        ><button
          :disabled="offset + 500 >= total || busy"
          @click="
            offset += 500;
            task(load);
          "
        >
          下一页
        </button>
      </div>
      <details v-if="legacyEvents.length">
        <summary>待排 / 事件规则 · {{ legacyEvents.length }} 项</summary>
        <p v-for="item in legacyEvents" :key="item.id" class="life-meta">
          {{ item.title }} · 等待真实事件
        </p>
      </details>
    </section>
    <section
      v-if="editor && tab === 'calendar'"
      id="schedule-editor"
      class="panel life-confirm"
      aria-label="活动编辑"
    >
      <h3>{{ selected ? "查看与修改活动" : "新建活动" }}</h3>
      <section
        v-if="selected?.state === 'pending_approval'"
        class="life-evidence"
        aria-label="人工批准日程"
      >
        <h4>批准已保存的系列</h4>
        <strong>{{ selected.definition.title }}</strong>
        <p class="life-prose">{{ selected.definition.purpose }}</p>
        <p>
          {{ displayTime(selected.definition.start_at, safeZone) }} —
          {{ displayTime(selected.definition.end_at, safeZone) }} · v{{
            selected.revision
          }}
        </p>
        <p class="life-meta">
          此批准针对上面已保存的完整系列与例外，不包含下方尚未保存的修改。阅读仍受本来源每日默认
          8 次模型请求与 24,000 预留 token
          预算约束；不扩大资料范围或对外联系权限。
        </p>
        <label class="life-check"
          ><input
            v-model="approvalConfirmed"
            type="checkbox"
          />我已核对定义与来源，明确批准整个已保存系列</label
        >
        <button
          class="primary"
          :disabled="busy || !approvalConfirmed"
          @click="approve"
        >
          批准已保存系列
        </button>
      </section>
      <form class="life-form" @submit.prevent="save">
        <div class="life-fields">
          <label
            >活动名称<input
              v-model="form.title"
              required
              maxlength="120" /></label
          ><label
            >活动类型<select v-model="form.type">
              <option
                v-for="kind in [
                  'reading',
                  'video',
                  'consolidation',
                  'chat',
                  'action',
                ]"
                :key="kind"
                :value="kind"
              >
                {{ activityLabels[kind] }}
              </option>
            </select></label
          ><label class="wide"
            >目的<textarea
              v-model="form.purpose"
              required
              rows="2"
              maxlength="2000"
            ></textarea></label
          ><label
            >计划开始<input
              v-model="form.start"
              type="datetime-local"
              required /></label
          ><label
            >计划结束<input
              v-model="form.end"
              type="datetime-local"
              required /></label
          ><label v-if="['reading', 'video'].includes(form.type)"
            >资料<select v-model="form.resource">
              <option value="">请选择授权资料</option>
              <option v-for="r in activeResources" :key="r.id" :value="r.id">
                {{ r.title }}
              </option>
            </select></label
          ><label
            >自主授权<select v-model="form.grant">
              <option value="">本次手动批准</option>
              <option v-for="g in activeGrants" :key="g.id" :value="g.id">
                {{ g.policy.title }}
              </option>
            </select></label
          ><label
            >重复<select v-model="form.frequency">
              <option value="">单次</option>
              <option value="daily">每天</option>
              <option value="weekly">每周</option>
              <option value="monthly">每月</option>
            </select></label
          ><label v-if="form.frequency"
            >次数<input
              v-model.number="form.repeatCount"
              type="number"
              min="1"
              max="365"
              required /></label
          ><label v-if="selected?.definition.repeat"
            >修改范围<select v-model="editScope">
              <option value="single">仅本次</option>
              <option value="following">本次及以后</option>
              <option value="series">整个系列</option>
            </select></label
          ><label class="life-check"
            ><input
              v-model="form.movable"
              type="checkbox"
            />允许在授权范围内调整时间</label
          >
        </div>
        <p class="life-meta">
          {{ timeZone }} ·
          视频理解与主动投递须单独验收，缺少能力时活动保留并显示阻断原因。
        </p>
        <label v-if="form.type === 'reading'" class="life-check"
          ><input
            v-model="form.restart"
            type="checkbox"
          />从头重读（默认接着该资料已完成的片段继续）</label
        >
        <div class="life-actions">
          <button class="primary" :disabled="busy">保存日程</button
          ><button type="button" :disabled="busy" @click="previewActivity">
            预演冲突与机会</button
          ><button type="button" @click="editor = false">关闭</button
          ><template v-if="selected"
            ><button
              type="button"
              :disabled="busy"
              @click="
                changeState(selected.state === 'paused' ? 'resume' : 'pause')
              "
            >
              {{ selected.state === "paused" ? "恢复" : "暂停" }}</button
            ><button
              type="button"
              :disabled="busy || selected.state === 'cancelled'"
              @click="changeState('cancel')"
            >
              取消活动
            </button></template
          >
        </div>
      </form>
      <div v-if="preview" class="life-evidence">
        <p>
          {{ preview.occurrences.length }} 次机会 ·
          {{ preview.conflicts.length }} 项冲突
        </p>
        <p v-if="preview.capability_block" class="life-meta">
          阻断：{{ blockReason(preview.capability_block) }}
        </p>
      </div>
      <div v-if="selected?.occurrences.length" class="life-evidence">
        <h4>实际进度与结果</h4>
        <article
          v-for="o in selected.occurrences"
          :key="o.id"
          class="life-evidence"
        >
          <strong
            >{{ displayTime(o.start_at, safeZone) }} ·
            {{ label(o.status) }}</strong
          >
          <p v-if="o.total_chars">
            实际处理 {{ o.processed_chars }} / {{ o.total_chars }} 字
          </p>
          <p v-if="o.block_reason">{{ blockReason(o.block_reason) }}</p>
          <small
            >实际开始 {{ displayTime(o.started_at, safeZone) }} · 完成
            {{ displayTime(o.completed_at, safeZone) }}</small
          >
          <pre v-if="o.result" class="life-prose">{{
            JSON.stringify(o.result, null, 2)
          }}</pre>
        </article>
      </div>
      <details v-if="form.type === 'reading'" class="reading-materials">
        <summary>阅读资料</summary>
        <p class="life-meta">
          可选择已有文字资料，或在这里加入有权提供的内容。资料只属于当前来源，阅读进度仅记录实际处理的片段与笔记。
        </p>
        <div class="life-cards">
          <article v-for="r in resources" :key="r.id" class="life-evidence">
            <strong>{{ r.title }}</strong>
            <span
              >{{ r.characters ?? 0 }} 字 ·
              {{ r.revoked_at ? "已撤销" : "已授权" }}</span
            >
            <small v-if="r.progress" class="life-meta"
              >已读 {{ r.progress.processed_chars }} /
              {{ r.progress.total_chars }} 字</small
            >
            <button
              v-if="!r.revoked_at"
              :disabled="busy"
              @click="revokeResource(r)"
            >
              撤销资料授权
            </button>
          </article>
        </div>
        <form class="life-form" @submit.prevent="uploadResource">
          <label
            >资料名称<input
              v-model="resourceForm.title"
              required
              maxlength="120"
          /></label>
          <label
            >正文<textarea
              v-model="resourceForm.content"
              rows="5"
              required
              maxlength="500000"
            ></textarea>
          </label>
          <label class="life-check"
            ><input
              v-model="resourceForm.allow"
              type="checkbox"
              required
            />我有权提供这些资料，并允许当前配置的模型在阅读活动中处理它们。</label
          >
          <button class="primary" :disabled="busy || !resourceForm.allow">
            保存阅读资料
          </button>
        </form>
      </details>
    </section>
    <template v-if="tab === 'autonomy'"
      ><section class="panel">
        <h3>工作目标</h3>
        <p class="life-meta">
          只在已授权的资料、时间窗和预算内安排阅读，交互消息优先处理。
        </p>
        <article v-for="p in plans" :key="p.id" class="life-evidence">
          <strong>{{ p.definition.goal }}</strong
          ><small
            >{{ label(p.state) }} · 最近规划
            {{ displayTime(p.last_planned_at) }}</small
          ><small v-if="p.planning_diagnostics"
            >{{ planningStatus(p.planning_diagnostics.status)
            }}<template v-if="p.planning_diagnostics.last_attempt?.error_code">
              ·
              {{
                blockReason(p.planning_diagnostics.last_attempt.error_code)
              }}</template
            ></small
          ><button :disabled="busy" @click="pausePlan(p)">
            {{ p.state === "paused" ? "恢复规划" : "暂停规划" }}
          </button>
        </article>
        <form class="life-form" @submit.prevent="createPlan">
          <label
            >工作目标<textarea
              v-model="planForm.goal"
              rows="2"
              required
              maxlength="2000"
            ></textarea></label
          ><label
            >使用授权<select v-model="planForm.grant">
              <option value="">请选择</option>
              <option v-for="g in activeGrants" :key="g.id" :value="g.id">
                {{ g.policy.title }}
              </option>
            </select></label
          >
          <fieldset class="life-fieldset">
            <legend>选择资料</legend>
            <label v-for="r in activeResources" :key="r.id" class="life-check"
              ><input
                v-model="planForm.resourceIds"
                :value="r.id"
                type="checkbox"
              />{{ r.title }}</label
            >
          </fieldset>
          <label
            >规划方式<select v-model="planForm.planningMode">
              <option value="bounded_rule">按目标有界排程</option>
              <option value="model">允许模型在授权内规划</option>
            </select></label
          ><label
            >每次预计分钟<input
              v-model.number="planForm.minutes"
              type="number"
              min="1"
              max="120"
              required /></label
          ><button
            class="primary"
            :disabled="busy || !planForm.grant || !planForm.resourceIds.length"
          >
            保存工作目标
          </button>
        </form>
      </section>
      <section class="panel">
        <h3>自主授权</h3>
        <article v-for="g in grants" :key="g.id" class="life-evidence">
          <strong>{{ g.policy.title }}</strong>
          <p>
            {{ g.policy.window_start }}–{{ g.policy.window_end }} · 每日
            {{ g.policy.max_daily_activities }} 项 /
            {{ g.policy.max_daily_minutes }} 分钟
          </p>
          <small
            >有效至 {{ displayTime(g.policy.valid_until) }} ·
            {{ g.revoked_at ? "已撤销" : "有效" }}</small
          ><button
            v-if="!g.revoked_at"
            :disabled="busy"
            @click="revokeGrant(g)"
          >
            撤销自主授权
          </button>
        </article>
        <form class="life-form" @submit.prevent="grant">
          <label
            >授权名称<input v-model="grantForm.title" required maxlength="120"
          /></label>
          <fieldset class="life-fieldset">
            <legend>允许阅读的资料</legend>
            <label v-for="r in activeResources" :key="r.id" class="life-check"
              ><input
                v-model="grantForm.resourceIds"
                :value="r.id"
                type="checkbox"
              />{{ r.title }}</label
            >
          </fieldset>
          <div class="life-fields">
            <label
              >每天开始<input
                v-model="grantForm.start"
                type="time"
                required /></label
            ><label
              >每天结束<input
                v-model="grantForm.end"
                type="time"
                required /></label
            ><label
              >有效天数<input
                v-model.number="grantForm.days"
                type="number"
                min="1"
                max="90"
                required /></label
            ><label
              >每日活动上限<input
                v-model.number="grantForm.dailyCount"
                type="number"
                min="1"
                max="24"
                required /></label
            ><label
              >每日分钟上限<input
                v-model.number="grantForm.dailyMinutes"
                type="number"
                min="1"
                max="480"
                required /></label
            ><label
              >每日模型调用上限<input
                v-model.number="grantForm.modelCalls"
                type="number"
                min="1"
                max="100"
                required /></label
            ><label
              >每日模型 token 预算<input
                v-model.number="grantForm.modelTokens"
                type="number"
                min="2000"
                max="200000"
                required
            /></label>
          </div>
          <p class="life-meta">
            授权只覆盖当前来源的阅读与规划，不授予对外联系或其他资料访问权限。
          </p>
          <button
            class="primary"
            :disabled="busy || !grantForm.resourceIds.length"
          >
            授予自主阅读范围
          </button>
        </form>
      </section>
    </template>
  </div>
</template>

<style scoped>
.calendar-heading {
  align-items: center;
}
.calendar-heading .life-actions {
  margin: 0;
}
.schedule-calendar {
  display: grid;
  grid-template-columns: repeat(7, minmax(0, 1fr));
  border: 1px solid var(--line);
  border-radius: 12px;
  overflow: hidden;
}
.calendar-day {
  min-height: 165px;
  border-right: 1px solid var(--line);
  border-bottom: 1px solid var(--line);
  background: #fff;
  padding: 7px;
  min-width: 0;
}
.calendar-day.today {
  background: #f4f6fc;
}
.calendar-view-week .calendar-day {
  min-height: 360px;
}
.calendar-day-label {
  display: flex;
  justify-content: space-between;
  width: 100%;
  border: none;
  padding: 6px 3px !important;
  font-size: 11px;
  background: transparent;
}
.calendar-day-label small {
  color: var(--muted);
  font-size: 10px;
}
.schedule-system .calendar-event {
  white-space: normal;
  width: 100%;
  border: 1px solid #d9e1ef;
  border-left: 3px solid #8c9fc7;
  background: #edf2fb;
  padding: 8px !important;
  text-align: left;
  display: grid;
  gap: 5px;
  margin-top: 6px;
  border-radius: 6px;
  overflow-wrap: anywhere;
}
.calendar-event strong {
  font-size: 11px;
  font-weight: 600;
}
.calendar-event > span,
.calendar-event small {
  font-size: 9px;
  color: #5d6d8d;
}
.schedule-system .event-consolidation {
  background: #f2f1f8;
  border-left-color: #a49cc6;
}
.schedule-system .event-chat {
  background: #f8f2e9;
  border-left-color: #bea170;
}
.schedule-system .event-legacy_trigger {
  background: #f5f6f8;
  border-left-color: #aab1bf;
}
.calendar-day.calendar-day {
  overflow-wrap: anywhere;
}
.schedule-calendar.calendar-view-day {
  display: block;
}
.calendar-view-week,
.calendar-view-day {
  position: relative;
  padding-left: 42px;
}
.calendar-view-week .calendar-day,
.calendar-view-day .calendar-day {
  min-height: 924px;
  position: relative;
  background-image: repeating-linear-gradient(
    to bottom,
    transparent 0,
    transparent 35px,
    #edf0f6 35px,
    #edf0f6 36px
  );
  background-position: 0 44px;
  background-repeat: repeat;
}
.calendar-view-week .calendar-day-label,
.calendar-view-day .calendar-day-label {
  position: absolute;
  top: 6px;
  left: 7px;
  width: calc(100% - 14px);
}
.calendar-view-week .calendar-event,
.calendar-view-day .calendar-event {
  position: absolute;
  left: 6px;
  width: calc(100% - 12px);
  margin: 0;
  z-index: 1;
  white-space: normal;
}
.calendar-hours {
  position: absolute;
  left: 0;
  top: 0;
  bottom: 0;
  width: 42px;
  background: #fafbfe;
  border-right: 1px solid var(--line);
}
.calendar-hours span {
  position: absolute;
  left: 5px;
  color: var(--muted);
  font-size: 9px;
}
.schedule-agenda {
  display: grid;
  gap: 24px;
}
.agenda-day h3 {
  font-size: 13px;
  padding-bottom: 10px;
  border-bottom: 1px solid var(--line);
}
.schedule-system .agenda-item {
  white-space: normal;
  display: grid;
  width: 100%;
  grid-template-columns: 150px 1fr;
  gap: 18px;
  text-align: left;
  border: 0;
  border-bottom: 1px solid var(--line);
  padding: 16px 4px;
  border-radius: 0;
}
.agenda-item span {
  display: grid;
  gap: 5px;
}
.agenda-item small {
  color: var(--muted);
  font-size: 11px;
}
.life-fieldset {
  border: 1px solid var(--line);
  border-radius: 10px;
  display: grid;
  gap: 12px;
  padding: 16px;
}
.life-fieldset legend {
  font-size: 12px;
  padding: 0 6px;
}
.calendar-panel > details,
.reading-materials {
  margin-top: 22px;
}
@media (max-width: 1000px) {
  .schedule-calendar {
    min-width: 720px;
  }
  .calendar-panel {
    overflow-x: auto;
  }
  .schedule-calendar.calendar-view-day {
    min-width: 0;
  }
  .agenda-item {
    grid-template-columns: 100px 1fr !important;
  }
}
</style>
