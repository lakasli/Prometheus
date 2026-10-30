<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { api } from "../api";
import ActivityEditor from "./ActivityEditor.vue";
import {
  activityTypes,
  scheduleStatus,
  scheduleReason,
} from "../schedule-labels";
const props = defineProps<{ refreshToken?: unknown }>();
const emit = defineEmits<{ error: [unknown]; dirty: [boolean] }>(),
  view = ref("month"),
  date = ref(
    new Intl.DateTimeFormat("en-CA", { timeZone: "Asia/Shanghai" }).format(
      new Date(),
    ),
  ),
  timeZone = ref("Asia/Shanghai"),
  result = ref<any>(null),
  busy = ref(false),
  editor = ref<InstanceType<typeof ActivityEditor>>();
const dragging = ref<any>(null);
let epoch = 0,
  controller: AbortController | undefined;
const cells = computed(() =>
  (result.value?.cells ?? []).map((cell: any) => ({
    ...cell,
    items: result.value.items.filter((item: any) =>
      item.end_at === null
        ? item.start_at >= cell.start_at && item.start_at < cell.end_at
        : item.start_at < cell.end_at && item.end_at > cell.start_at,
    ),
  })),
);
async function load() {
  controller?.abort();
  controller = new AbortController();
  const signal = controller.signal,
    version = ++epoch;
  busy.value = true;
  try {
    const query = new URLSearchParams({
      date: date.value,
      view: view.value,
      time_zone: timeZone.value,
    });
    const data = await api<any>(
      `/schedule/calendar?${query}`,
      "GET",
      undefined,
      undefined,
      signal,
    );
    if (version === epoch) result.value = data;
  } catch (error) {
    if (!signal.aborted) emit("error", error);
  } finally {
    if (version === epoch) busy.value = false;
  }
}
function move(direction: number) {
  const current = new Date(`${date.value}T00:00:00Z`);
  if (view.value === "month") {
    current.setUTCDate(1);
    current.setUTCMonth(current.getUTCMonth() + direction);
  } else
    current.setUTCDate(
      current.getUTCDate() + direction * (view.value === "week" ? 7 : 1),
    );
  date.value = current.toISOString().slice(0, 10);
  void load();
}
async function drop(cell: any) {
  const item = dragging.value;
  dragging.value = null;
  if (!item || item.read_only) return;
  try {
    const response = await api<any>(
      `/schedule/activities/${item.activity_id}/move-preview`,
      "POST",
      {
        nominal_at: item.nominal_at,
        target_date: cell.key,
        time_zone: result.value?.time_zone ?? timeZone.value,
      },
      item.revision,
    );
    await editor.value?.suggestMove(item, response);
  } catch (error) {
    emit("error", error);
  }
}
onMounted(load);
defineExpose({ refresh: load });
watch(
  () => props.refreshToken,
  () => void load(),
);
onUnmounted(() => controller?.abort());
</script>
<template>
  <section class="panel feature-section">
    <div class="section-heading">
      <div>
        <h2>日历</h2>
        <p>
          计划、实际执行和只读系统记录分别显示。当前应用时区：{{
            result?.time_zone || timeZone
          }}
        </p>
        <p class="muted">拖动活动只会预演新日期，核对后须另行保存。</p>
      </div>
      <div class="calendar-controls">
        <button class="secondary" aria-label="上一时间范围" @click="move(-1)">
          ←</button
        ><label
          ><input
            v-model="date"
            aria-label="日历日期"
            type="date"
            @change="load" /></label
        ><select v-model="view" aria-label="日历视图" @change="load">
          <option value="month">月</option>
          <option value="week">周</option>
          <option value="day">日</option></select
        ><button class="secondary" aria-label="下一时间范围" @click="move(1)">
          →</button
        ><button class="secondary" :disabled="busy" @click="load">
          刷新日历
        </button>
      </div>
    </div>
    <label class="calendar-zone"
      >日历显示时区<input v-model="timeZone" maxlength="128" @change="load"
    /></label>
    <p v-if="busy" class="muted" role="status">正在读取日历</p>
    <div class="calendar-grid" :class="{ single: view === 'day' }">
      <article
        v-for="cell in cells"
        :key="cell.key"
        class="calendar-day"
        :class="{ muted: !cell.current }"
        @dragover.prevent
        @drop.prevent="drop(cell)"
      >
        <time :datetime="cell.key">{{ cell.number }}</time>
        <p v-if="cell.start_at === cell.end_at" class="muted">当地跳过此日期</p>
        <div v-for="item in cell.items" :key="item.id" class="calendar-entry">
          <button
            v-if="item.activity_id"
            class="calendar-event"
            :draggable="!item.read_only"
            @dragstart="dragging = item"
            @dragend="dragging = null"
            @click="editor?.open(item)"
          >
            {{ item.title }}</button
          ><span v-else>{{ item.title }}</span
          ><small
            >{{ scheduleStatus(item.status)
            }}{{ item.end_at === null ? " · 未指定时长" : "" }}</small
          >
        </div>
      </article>
    </div>
    <details v-if="result">
      <summary>此范围议程（{{ result.total }}）</summary>
      <article v-for="item in result.items" :key="item.id" class="job-card">
        <strong>{{ item.title }}</strong>
        <p>
          {{ activityTypes[item.type] || "系统记录" }} ·
          {{ scheduleStatus(item.status) }}
        </p>
        <p>
          {{
            new Intl.DateTimeFormat("zh-CN", {
              timeZone: result.time_zone,
              dateStyle: "short",
              timeStyle: "short",
            }).format(new Date(item.start_at * 1000))
          }}
        </p>
        <p>{{ scheduleReason(item.block_reason) }}</p>
        <button
          v-if="item.activity_id"
          class="secondary"
          @click="editor?.open(item)"
        >
          管理这个机会</button
        ><small v-else class="muted"
          >只读投影，请在原日程、触发任务或整理记录中管理。</small
        >
      </article>
    </details>
    <details
      v-if="result?.unscheduled_rules.length || result?.blocked_rules.length"
    >
      <summary>未排定的触发规则</summary>
      <p v-for="item in result.unscheduled_rules" :key="item.id">
        {{ item.title }} ·
        {{ item.type === "event" ? "等待事件" : "尚未排定" }} ·
        {{ item.enabled ? "启用" : "停用" }}
      </p>
      <p v-for="item in result.blocked_rules" :key="item.id">
        {{ item.title }} · 该年度工作日日历尚未核验
      </p>
    </details>
  </section>
  <ActivityEditor
    ref="editor"
    :date="date"
    :time-zone="timeZone"
    @changed="load"
    @error="emit('error', $event)"
    @dirty="emit('dirty', $event)"
  />
</template>
