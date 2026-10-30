<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>(),
  view = ref("month"),
  date = ref(
    new Intl.DateTimeFormat("en-CA", { timeZone: "Asia/Shanghai" }).format(
      new Date(),
    ),
  ),
  items = ref<any[]>([]),
  busy = ref(false);
const cells = computed(() => {
  const chosen = new Date(`${date.value}T00:00:00Z`),
    first = new Date(chosen);
  if (view.value === "month") first.setUTCDate(1);
  if (view.value !== "day")
    first.setUTCDate(first.getUTCDate() - ((first.getUTCDay() + 6) % 7));
  const count = view.value === "day" ? 1 : view.value === "week" ? 7 : 42;
  return Array.from({ length: count }, (_, offset) => {
    const value = new Date(first.getTime() + offset * 86400000),
      key = value.toISOString().slice(0, 10),
      start = Date.parse(`${key}T00:00:00+08:00`) / 1000;
    return {
      key,
      number: value.getUTCDate(),
      current: value.getUTCMonth() === chosen.getUTCMonth(),
      items: items.value.filter(
        (item) => item.start_at < start + 86400 && item.end_at > start,
      ),
    };
  });
});
async function load() {
  busy.value = true;
  try {
    const first = cells.value[0]!.key,
      last = cells.value.at(-1)!.key;
    items.value = (
      await api<any>(
        `/automation/calendar?start=${Date.parse(`${first}T00:00:00+08:00`) / 1000}&end=${Date.parse(`${last}T00:00:00+08:00`) / 1000 + 86400}`,
      )
    ).items;
  } catch (error) {
    emit("error", error);
  } finally {
    busy.value = false;
  }
}
function move(direction: number) {
  const current = new Date(`${date.value}T00:00:00Z`);
  if (view.value === "month")
    current.setUTCMonth(current.getUTCMonth() + direction);
  else
    current.setUTCDate(
      current.getUTCDate() + direction * (view.value === "week" ? 7 : 1),
    );
  date.value = current.toISOString().slice(0, 10);
  void load();
}
onMounted(load);
</script>
<template>
  <section class="panel feature-section">
    <div class="section-heading">
      <div>
        <h2>日历</h2>
        <p>上海时间，按真实日程显示。</p>
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
    <div class="calendar-grid" :class="{ single: view === 'day' }">
      <article
        v-for="cell in cells"
        :key="cell.key"
        class="calendar-day"
        :class="{ muted: !cell.current }"
      >
        <time :datetime="cell.key">{{ cell.number }}</time>
        <p v-for="item in cell.items" :key="item.id" class="calendar-entry">
          {{ item.title }}
          <small>{{
            {
              planned: "已安排",
              in_progress: "进行中",
              completed: "已完成",
              cancelled: "已取消",
            }[item.status as string]
          }}</small>
        </p>
      </article>
    </div>
  </section>
</template>
