<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { api } from "../api";
import {
  activityTypes,
  scheduleReason,
  scheduleStatus,
} from "../schedule-labels";
const props = defineProps<{ date: string; timeZone: string }>(),
  emit = defineEmits<{ changed: []; error: [unknown]; dirty: [boolean] }>();
const opened = ref(false),
  busy = ref(false),
  notice = ref(""),
  detail = ref<any>(null),
  selected = ref<any>(null),
  preview = ref<any>(null),
  items = ref<any[]>([]),
  resources = ref<any[]>([]),
  attempts = ref<any[]>([]),
  offset = ref(0),
  total = ref(0),
  scope = ref("series"),
  baseline = ref("");
const form = reactive({
  title: "",
  type: "reading",
  purpose: "",
  start: "",
  end: "",
  time_zone: "Asia/Shanghai",
  resource_id: "",
  target_ref: "",
  frequency: "",
  interval: 1,
  count: 0,
  until: "",
  restart_reading: false,
  movable: false,
  grant_id: null as string | null,
  reset: false,
  state: "enabled",
});
let requestKey = crypto.randomUUID(),
  previousScope = "series";
const dirty = computed(
  () => busy.value || (opened.value && JSON.stringify(form) !== baseline.value),
);
watch(dirty, (value) => emit("dirty", value));
onUnmounted(() => emit("dirty", false));
const needsNominal = computed(() => scope.value !== "series");
async function act(action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  notice.value = "";
  try {
    await action();
  } catch (error) {
    emit("error", error);
  } finally {
    busy.value = false;
  }
}
async function load() {
  const [list, books] = await Promise.all([
    api<any>(`/schedule/activities?limit=20&offset=${offset.value}`),
    api<any>("/schedule/resources"),
  ]);
  items.value = list.items;
  total.value = list.total;
  resources.value = books.items.filter((item: any) => !item.revoked_at);
}
function populate() {
  const data = detail.value?.definition,
    slot = needsNominal.value ? selected.value : null;
  const value = slot?.definition ?? data;
  Object.assign(
    form,
    value
      ? {
          title: value.title,
          type: value.type,
          purpose: value.purpose,
          start: slot?.local_start ?? detail.value.local_start,
          end: slot?.local_end ?? detail.value.local_end,
          time_zone: value.time_zone,
          resource_id: value.resource_id ?? "",
          target_ref: value.target_ref ?? "",
          frequency:
            scope.value === "single"
              ? ""
              : ((scope.value === "following"
                  ? selected.value?.following_repeat
                  : value.repeat
                )?.frequency ?? ""),
          interval:
            (scope.value === "following"
              ? selected.value?.following_repeat
              : value.repeat
            )?.interval ?? 1,
          count:
            (scope.value === "following"
              ? selected.value?.following_repeat
              : value.repeat
            )?.count ?? 0,
          until:
            scope.value === "single" ? "" : (detail.value.local_until ?? ""),
          restart_reading: value.restart_reading,
          movable: value.movable,
          grant_id: value.grant_id,
          reset: false,
          state: detail.value.state,
        }
      : {
          title: "",
          type: "reading",
          purpose: "",
          start: `${props.date}T09:00:00`,
          end: `${props.date}T09:30:00`,
          time_zone: props.timeZone,
          resource_id: "",
          target_ref: "",
          frequency: "",
          interval: 1,
          count: 0,
          until: "",
          restart_reading: false,
          movable: false,
          grant_id: null,
          reset: false,
          state: "enabled",
        },
  );
  baseline.value = JSON.stringify(form);
  preview.value = null;
  previousScope = scope.value;
}
async function open(item: any = null) {
  if (busy.value) return false;
  if (dirty.value && !confirm("放弃尚未保存的活动编辑？")) return false;
  let loaded = false;
  await act(async () => {
    detail.value = item
      ? await api<any>(`/schedule/activities/${item.activity_id ?? item.id}`)
      : null;
    selected.value = item?.nominal_at !== undefined ? item : null;
    scope.value = selected.value ? "single" : "series";
    opened.value = true;
    requestKey = crypto.randomUUID();
    attempts.value = [];
    await load();
    populate();
    loaded = true;
  });
  return loaded;
}
async function suggestMove(item: any, result: any) {
  if (!(await open(item))) return;
  form.start = result.local_start;
  form.end = result.local_end;
  preview.value = result.preview;
  notice.value = "拖动改期已预演，核对冲突和时间后保存才会生效。";
}
defineExpose({ open, suggestMove });
function changeScope() {
  if (
    JSON.stringify(form) !== baseline.value &&
    !confirm("切换修改范围会重新载入活动字段，确定继续？")
  ) {
    scope.value = previousScope;
    return;
  }
  populate();
}
function definition() {
  return {
    title: form.title,
    type: form.type,
    purpose: form.purpose,
    start_local: form.start,
    end_local: form.end,
    time_zone: form.time_zone,
    resource_id: form.type === "reading" ? form.resource_id || null : null,
    target_ref: form.type === "chat" ? form.target_ref || null : null,
    grant_id: form.grant_id,
    repeat: form.frequency
      ? {
          frequency: form.frequency,
          interval: form.interval,
          count: form.count || null,
          until_local: form.until || null,
        }
      : null,
    restart_reading: form.restart_reading,
    movable: form.movable,
  };
}
function changeBody() {
  return {
    definition: definition(),
    scope: scope.value,
    ...(needsNominal.value ? { nominal_at: selected.value?.nominal_at } : {}),
    reset_future_exceptions: form.reset,
  };
}
async function check() {
  await act(async () => {
    preview.value = detail.value
      ? await api<any>(
          `/schedule/activities/${detail.value.id}/preview`,
          "POST",
          changeBody(),
          detail.value.revision,
        )
      : await api<any>("/schedule/activities/preview", "POST", {
          definition: definition(),
        });
  });
}
async function save() {
  await act(async () => {
    if (detail.value) {
      const result = await api<any>(
        `/schedule/activities/${detail.value.id}`,
        "PATCH",
        changeBody(),
        detail.value.revision,
      );
      detail.value = await api<any>(
        `/schedule/activities/${result.following_id ?? detail.value.id}`,
      );
      notice.value = result.following_id
        ? "旧系列已截断，后续系列已另建。"
        : "活动更改已保存。";
    } else {
      const result = await api<any>(
        "/schedule/activities",
        "POST",
        { definition: definition(), state: form.state },
        undefined,
        undefined,
        requestKey,
      );
      detail.value = await api<any>(`/schedule/activities/${result.id}`);
      notice.value = "活动已保存，执行与投递以实际记录为准。";
    }
    selected.value = null;
    scope.value = "series";
    populate();
    await load();
    emit("changed");
  });
}
async function state(action: string) {
  if (
    !detail.value ||
    !confirm(
      `确认${{ pause: "暂停", resume: "恢复", cancel: "取消" }[action]}所选范围？已产生的真实记录保留。`,
    )
  )
    return;
  await act(async () => {
    const result = await api<any>(
      `/schedule/activities/${detail.value.id}/${action}`,
      "POST",
      {
        scope: scope.value,
        ...(needsNominal.value
          ? { nominal_at: selected.value?.nominal_at }
          : {}),
      },
      detail.value.revision,
    );
    detail.value = result.following_id
      ? await api<any>(`/schedule/activities/${result.following_id}`)
      : result;
    if (result.following_id) {
      selected.value = null;
      scope.value = "series";
    }
    populate();
    await load();
    notice.value = "活动状态已保存。";
    emit("changed");
  });
}
async function approve() {
  if (
    !detail.value ||
    !confirm(
      "批准当前保存的整个系列及所有例外？这不会启用未验证的视频、联系或动作能力。",
    )
  )
    return;
  await act(async () => {
    detail.value = await api<any>(
      `/schedule/activities/${detail.value.id}/approve`,
      "POST",
      { scope: "series", confirmed: true },
      detail.value.revision,
    );
    populate();
    await load();
    notice.value = "保存的系列已批准，实际执行仍受能力门禁约束。";
    emit("changed");
  });
}
async function history() {
  await act(async () => {
    const [events, current] = await Promise.all([
      api<any>(`/schedule/activities/${detail.value.id}/attempts`),
      api<any>(`/schedule/activities/${detail.value.id}`),
    ]);
    attempts.value = events.items;
    detail.value.occurrences = current.occurrences;
    if (current.revision !== detail.value.revision)
      notice.value =
        "当前编辑基准已有新版本，请重新载入后核对，旧表单尚未应用。";
  });
}
function close() {
  if (!dirty.value || confirm("放弃尚未保存的活动编辑？")) opened.value = false;
}
onMounted(() => void load().catch((error) => emit("error", error)));
</script>
<template>
  <section class="panel feature-section schedule-editor">
    <div class="section-heading">
      <div>
        <h2>系列与活动</h2>
        <p>
          计划、实际覆盖和投递分别记账；视频、主动联系和动作仍须完成各自验收。
        </p>
      </div>
      <button class="primary" :disabled="busy" @click="open()">新建活动</button>
    </div>
    <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
    <details>
      <summary>已保存的活动（{{ total }}）</summary>
      <article v-for="item in items" :key="item.id" class="job-card">
        <strong>{{ item.definition.title }}</strong>
        <p>
          {{ activityTypes[item.definition.type] }} ·
          {{ scheduleStatus(item.state) }} · {{ item.local_start }} ·
          {{ item.definition.time_zone }}
        </p>
        <p class="muted">{{ scheduleReason(item.capability_block) }}</p>
        <button class="secondary" :disabled="busy" @click="open(item)">
          管理此活动
        </button>
      </article>
      <div class="form-actions">
        <button
          class="secondary"
          :disabled="busy || offset === 0"
          @click="
            offset -= 20;
            act(load);
          "
        >
          上一组活动</button
        ><button
          class="secondary"
          :disabled="busy || offset + 20 >= total"
          @click="
            offset += 20;
            act(load);
          "
        >
          下一组活动</button
        ><button class="secondary" :disabled="busy" @click="act(load)">
          刷新活动与资料
        </button>
      </div>
    </details>
    <form v-if="opened" @submit.prevent="save">
      <fieldset :disabled="busy">
        <p v-if="detail" class="muted">
          {{ scheduleStatus(detail.state) }} · 版本 {{ detail.revision }} ·
          {{ detail.creator === "autonomous" ? "自主安排" : "人工活动" }}
        </p>
        <label v-if="selected"
          >修改范围<select
            v-model="scope"
            aria-label="活动修改范围"
            @change="changeScope"
          >
            <option value="single">仅这一次</option>
            <option value="following">这一次及此后</option>
            <option value="series">整个系列</option>
          </select></label
        >
        <label
          >活动标题<input
            v-model="form.title"
            maxlength="240"
            required /></label
        ><label
          >活动类型<select v-model="form.type" aria-label="活动类型">
            <option
              v-for="key in [
                'reading',
                'writing',
                'video',
                'consolidation',
                'chat',
                'action',
              ]"
              :key="key"
              :value="key"
            >
              {{ activityTypes[key] }}
            </option>
          </select></label
        >
        <label
          >活动目的<textarea
            v-model="form.purpose"
            maxlength="4000"
            rows="2"
            required
          ></textarea></label
        ><label
          >活动时区<input v-model="form.time_zone" maxlength="128" required
        /></label>
        <div class="form-row">
          <label
            >活动开始<input
              v-model="form.start"
              type="datetime-local"
              step="1"
              required /></label
          ><label
            >活动结束<input
              v-model="form.end"
              type="datetime-local"
              step="1"
              required
          /></label>
        </div>
        <p class="muted">
          重复保持所选时区的本地时刻；夏令时不存在或歧义的时刻会跳过。跨日活动请分开安排。
        </p>
        <label v-if="form.type === 'reading'"
          >阅读资料<select
            v-model="form.resource_id"
            aria-label="活动阅读资料"
            required
          >
            <option value="" disabled>先在下方明确授权文本</option>
            <option v-for="book in resources" :key="book.id" :value="book.id">
              {{ book.title }}
            </option>
          </select></label
        ><label v-if="form.type === 'reading'" class="checkbox"
          ><input
            v-model="form.restart_reading"
            type="checkbox"
          />本次从头重读，保留先前笔记</label
        >
        <label v-if="form.type === 'chat'"
          >固定联系目标引用<input
            v-model="form.target_ref"
            maxlength="256"
            required
        /></label>
        <template v-if="scope !== 'single'"
          ><label
            >活动重复<select v-model="form.frequency" aria-label="活动重复">
              <option value="">不重复</option>
              <option value="daily">每天</option>
              <option value="weekly">每周</option>
              <option value="monthly">每月</option>
            </select></label
          >
          <div v-if="form.frequency" class="form-row">
            <label
              >重复间隔<input
                v-model.number="form.interval"
                type="number"
                min="1"
                max="31"
                required /></label
            ><label
              >重复次数（0表示不指定）<input
                v-model.number="form.count"
                type="number"
                min="0"
                max="3660"
            /></label>
          </div>
          <label v-if="form.frequency"
            >重复截至<input v-model="form.until" type="datetime-local" step="1"
          /></label>
          <p v-if="form.frequency" class="muted">
            不存在的月日期会跳过，次数包含被跳过的机会；只按有界范围展开。
          </p></template
        >
        <label
          v-if="detail?.exceptions.length && scope !== 'single'"
          class="checkbox"
          ><input
            v-model="form.reset"
            type="checkbox"
          />已核对：本次系列改期可以重置未来例外</label
        >
        <label v-if="!detail"
          >保存后的活动状态<select v-model="form.state" aria-label="新活动状态">
            <option value="enabled">启用</option>
            <option value="paused">暂停</option>
          </select></label
        >
        <div class="form-actions">
          <button type="button" class="secondary" @click="check">
            预演活动</button
          ><button class="primary">
            {{ detail ? "保存活动更改" : "保存活动" }}</button
          ><button type="button" class="secondary" @click="close">
            关闭活动编辑
          </button>
        </div>
      </fieldset>
    </form>
    <article v-if="preview" class="job-card">
      <strong>只读预演</strong>
      <p>
        未来展示 {{ preview.occurrences.length }} 个机会 · 冲突
        {{ preview.conflicts.length }} 项
      </p>
      <p v-for="(item, index) in preview.conflicts" :key="index">
        {{ item.title || "另一个私有安排的忙碌时间" }} ·
        {{ new Date(item.start_at * 1000).toLocaleString() }}
      </p>
      <p class="muted">{{ scheduleReason(preview.capability_block) }}</p>
    </article>
    <template v-if="detail && opened"
      ><div class="form-actions">
        <button class="secondary" :disabled="busy" @click="state('pause')">
          暂停所选范围</button
        ><button
          class="secondary"
          :disabled="busy || detail.state === 'pending_approval'"
          @click="state('resume')"
        >
          恢复所选范围</button
        ><button
          class="secondary danger"
          :disabled="busy"
          @click="state('cancel')"
        >
          取消所选范围</button
        ><button
          v-if="detail.state === 'pending_approval'"
          class="primary"
          :disabled="busy"
          @click="approve"
        >
          明确批准保存的整个系列
        </button>
      </div>
      <details>
        <summary>实际机会与执行记录</summary>
        <p v-if="!detail.occurrences.length" class="muted">
          尚无实际执行记录。计划时间不证明已执行。
        </p>
        <article
          v-for="item in detail.occurrences"
          :key="item.id"
          class="job-card"
        >
          <strong>{{ scheduleStatus(item.status) }}</strong>
          <p>{{ scheduleReason(item.block_reason) }}</p>
          <p v-if="item.total_chars != null">
            已提交覆盖 {{ item.checkpoint }} / {{ item.total_chars }} 字符
          </p>
        </article>
        <button class="secondary" :disabled="busy" @click="history">
          读取近期请求记录
        </button>
        <p v-for="item in attempts" :key="item.id">
          {{ scheduleStatus(item.status) }} ·
          {{ scheduleReason(item.error_code) }} · 已预留
          {{ item.reserved_tokens }} 预算
        </p>
      </details></template
    >
  </section>
</template>
