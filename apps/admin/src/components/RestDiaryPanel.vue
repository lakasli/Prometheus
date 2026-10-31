<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>(),
  items = ref<any[]>([]),
  busy = ref(false);
let disposed = false,
  controller: AbortController | undefined,
  timer: ReturnType<typeof setTimeout> | undefined;
const states: Record<string, string> = {
  frozen: "已冻结，等待收尾",
  preparing: "按页准备来源",
  model_wait: "等待或进行模型整理",
  completed: "本批请求已处理",
  partial: "保留部分结果与检查点",
  interrupted: "本批已中断",
  local: "已保存本地摘录或来源目录",
  pending: "等待处理",
  capturing: "正在采集冻结消息",
  planning: "正在按页登记历史周期",
  prepared: "来源已准备",
  blocked: "条件未满足",
  invalidated: "来源已失效",
};
const reasons: Record<string, string> = {
  legacy_rest_snapshot_missing: "旧版本没有冻结来源证据，本批不推测补造",
  rest_cycle_ended: "本次整理时段已结束，未完成部分保留检查点",
  rest_diary_source_changed: "来源许可、版本或删除水位已变化",
  rest_diary_reference_changed: "冻结资料已被更正、遗忘或投递状态改变",
  rest_diary_target_protected: "人工保护或遗忘的周期没有覆盖",
  rest_diary_target_changed: "冻结后周期版本已改变",
  daily_budget_wait: "等待模型预算恢复",
  daily_event_limit: "单日来源超过当前整理上限",
};
const periods: Record<string, string> = {
  day: "日记",
  week: "周记",
  month: "月记",
};
const count = (cycle: any, state: string) =>
  Number(cycle.target_counts?.[state] ?? 0);
const loadingTargets = ref(new Set<string>()),
  targetControllers = new Map<string, AbortController>(),
  historyPaused = ref(false);
let generation = 0;
async function more(cycle: any) {
  if (
    disposed ||
    busy.value ||
    !cycle.targets_next ||
    targetControllers.has(cycle.cycle_id)
  )
    return;
  const id = String(cycle.cycle_id),
    version = generation,
    abort = new AbortController();
  historyPaused.value = true;
  targetControllers.set(id, abort);
  loadingTargets.value = new Set(targetControllers.keys());
  try {
    const r = await api<any>(
      `/diary/rest-cycles/${id}/targets?before=${cycle.targets_next}&limit=100`,
      "GET",
      undefined,
      undefined,
      abort.signal,
    );
    if (
      !disposed &&
      !abort.signal.aborted &&
      generation === version &&
      items.value.includes(cycle)
    ) {
      const ids = new Set(cycle.targets.map((t: any) => t.cursor));
      cycle.targets.push(...r.items.filter((t: any) => !ids.has(t.cursor)));
      cycle.targets_next = r.next;
    }
  } catch (e) {
    if (
      !disposed &&
      generation === version &&
      !(e instanceof Error && e.name === "AbortError")
    )
      emit("error", e);
  } finally {
    if (targetControllers.get(id) === abort) targetControllers.delete(id);
    if (!disposed) loadingTargets.value = new Set(targetControllers.keys());
  }
}
async function load() {
  if (busy.value || disposed) return;
  busy.value = true;
  historyPaused.value = false;
  generation++;
  for (const c of targetControllers.values()) c.abort();
  targetControllers.clear();
  loadingTargets.value = new Set();
  controller?.abort();
  controller = new AbortController();
  try {
    const r = await api<any>(
      "/diary/rest-cycles",
      "GET",
      undefined,
      undefined,
      controller.signal,
    );
    if (!disposed && !controller.signal.aborted) items.value = r.items;
  } catch (e) {
    if (!disposed && !(e instanceof Error && e.name === "AbortError"))
      emit("error", e);
  } finally {
    if (!disposed) busy.value = false;
  }
}
function poll() {
  timer = setTimeout(async () => {
    if (
      !document.hidden &&
      !historyPaused.value &&
      items.value.some((c) =>
        ["frozen", "preparing", "model_wait"].includes(c.status),
      )
    )
      await load();
    if (!disposed) poll();
  }, 1500);
}
onMounted(() => {
  void load();
  poll();
});
onUnmounted(() => {
  disposed = true;
  controller?.abort();
  for (const c of targetControllers.values()) c.abort();
  targetControllers.clear();
  if (timer) clearTimeout(timer);
});
</script>
<template>
  <section class="panel feature-section rest-diary" aria-label="睡眠整理批次">
    <div class="section-heading">
      <div>
        <h2>睡眠整理批次</h2>
        <p>
          只显示你管理的来源。各自成文；本地摘录、模型归并和未完成部分分别记录。
        </p>
      </div>
      <button class="secondary" :disabled="busy" @click="load">
        刷新整理批次
      </button>
    </div>
    <p v-if="!items.length" class="muted">尚无已冻结的睡眠整理批次。</p>
    <p v-if="historyPaused" class="muted">
      已暂停自动刷新以便浏览历史；点击刷新整理批次可更新全部状态。
    </p>
    <article v-for="cycle in items" :key="cycle.cycle_id" class="job-card">
      <h3>{{ states[cycle.status] ?? "待核对" }}</h3>
      <p v-if="cycle.error_code">
        {{ reasons[cycle.error_code] ?? "本批仍有待核对的条件。" }}
      </p>
      <p v-else>
        冻结截至：{{
          new Date(cycle.frozen_at * 1000).toLocaleString("zh-CN", {
            timeZone: "Asia/Shanghai",
          })
        }}
      </p>
      <p>
        本页来源{{ cycle.sources.length }}个 · 本地摘录/目录{{
          count(cycle, "local")
        }}项 · 模型或无互动处理{{ count(cycle, "completed") }}项 · 等待{{
          count(cycle, "pending")
        }}项
      </p>
      <p v-if="cycle.request_counts?.pending" class="muted">
        另有{{
          cycle.request_counts.pending
        }}个周期待准备；已保存片段保留，原唤醒期限不变。
      </p>
      <p class="muted">
        冻结后的新消息留待后续处理；当天未结束或仅覆盖冻结时刻时，不代表全天完成。未获模型许可的来源仅在本地保存。
      </p>
      <details>
        <summary>查看来源与周期状态</summary>
        <article
          v-for="(source, sourceIndex) in cycle.sources"
          :key="source.policy_id"
          class="source-state"
        >
          <strong>{{
            source.source_key.startsWith("self:")
              ? "自己的明确记录"
              : `获授权会话来源 ${Number(sourceIndex) + 1}`
          }}</strong>
          <p>
            {{ source.visibility === "private" ? "私有分区" : "共享分区" }} ·
            {{ source.time_zone }} · {{ states[source.status] ?? "待核对" }} ·
            已处理{{ source.processed }}条，排除{{ source.excluded }}条
          </p>
          <p>
            {{ source.allow_model ? "本批已有模型出域许可" : "本批仅本地整理" }}
          </p>
          <p v-if="source.historical_backlog === 0" class="muted">
            本批沿用旧版冻结的日期范围；更早的获授权资料留待下次睡眠。
          </p>
          <p v-if="source.error_code">
            {{
              reasons[source.error_code] ?? "来源条件已变化，请核对日记记录。"
            }}
          </p>
          <details>
            <summary>查看本来源的日、周、月状态</summary>
            <p
              v-for="target in cycle.targets.filter(
                (t: any) => t.policy_id === source.policy_id,
              )"
              :key="`${target.period}:${target.date}`"
            >
              {{ target.date }} · {{ periods[target.period] }} ·
              {{ states[target.state] ?? "待核对"
              }}<span v-if="target.error_code">
                · {{ reasons[target.error_code] ?? "请核对条件和检查点" }}</span
              >
            </p>
          </details>
        </article>
        <p class="muted">
          已显示{{ cycle.targets.length }}项周期状态；上方计数包含本批全部周期。
        </p>
        <button
          v-if="cycle.targets_next"
          class="secondary"
          :disabled="busy || loadingTargets.has(cycle.cycle_id)"
          @click="more(cycle)"
        >
          加载更多周期状态
        </button>
      </details>
    </article>
  </section>
</template>
<style scoped>
.rest-diary,
.source-state {
  min-width: 0;
}
.rest-diary p {
  overflow-wrap: anywhere;
}
.source-state {
  border-top: 1px solid var(--line);
  padding: 12px 0;
}
.section-heading {
  gap: 12px;
  flex-wrap: wrap;
}
</style>
