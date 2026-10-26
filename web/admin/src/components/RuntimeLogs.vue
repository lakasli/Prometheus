<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { api, ApiError, errorMessage } from "../api";
import { formatDate } from "../validation";
import AdminSelect from "./AdminSelect.vue";
import {
  contextBlockLabels,
  selectionReasonLabels,
} from "../context-management";
import type { ContextSelectionView } from "../context-management";

interface RuntimeEvent {
  id: number;
  kind: string;
  status: string;
  source: string;
  run_id: string | null;
  conversation_id: string | null;
  title: string;
  content: string;
  details: Record<string, unknown>;
  created_at: number;
}
interface LogPage {
  items: RuntimeEvent[];
  next_cursor: string | null;
  capabilities: { subagent: boolean; asr: boolean; tts: boolean };
  retention: { days: number; max_events: number };
}
const emit = defineEmits<{ unauthorized: [] }>();
const kindLabels: Record<string, string> = {
  diary: "日记整理",
  relationships: "人物与关系",
  schedule: "日程执行",
  input: "输入",
  output: "输出",
  run: "运行状态",
  tool: "能力调用",
  subagent: "协作分工",
  tts: "语音合成",
  asr: "语音识别",
  vision: "图片理解",
  memory: "记忆",
  delivery: "消息投递",
  system: "系统",
};
const statusLabels: Record<string, string> = {
  active: "允许发布",
  suppressed: "旧候选已停止发布",
  recorded: "已记录",
  accepted: "已接受",
  unknown: "结果未知",
  queued: "排队中",
  running: "运行中",
  completed: "已完成",
  failed: "失败",
  cancelled: "已取消",
  interrupted: "已中断",
  outcome_unknown: "结果未知",
  received: "已接收",
  succeeded: "成功",
  sent: "已发送",
  pending: "待处理",
  blocked: "已阻止",
  expired: "已过期",
  delivered: "已投递",
  started: "已开始",
  available: "可用",
  unavailable: "不可用",
};
const filters = reactive({ kind: "", status: "", run_id: "", q: "" });
const applied = ref({ ...filters });
const items = ref<RuntimeEvent[]>([]);
const nextCursor = ref<string | null>(null);
const snapshot = ref<LogPage | null>(null);
const loading = ref(false);
const error = ref("");
const autoRefresh = ref(false);
const loadedAt = ref<number | null>(null);
const expanded = ref(new Set<number>());
const selections = reactive<
  Record<
    string,
    { loading: boolean; error: string; data: ContextSelectionView | null }
  >
>({});
const selectionControllers = new Map<string, AbortController>();
const hasUnapplied = computed(
  () => JSON.stringify(filters) !== JSON.stringify(applied.value),
);
let disposed = false;
let timer: ReturnType<typeof setTimeout> | undefined;
let controller: AbortController | undefined;
let epoch = 0;
function toggle(id: number) {
  const value = new Set(expanded.value);
  value.has(id) ? value.delete(id) : value.add(id);
  expanded.value = value;
}
function details(event: RuntimeEvent) {
  return JSON.stringify(event.details, null, 2);
}
async function loadSelection(run: string) {
  if (selections[run]?.loading || disposed) return;
  selections[run] = { loading: true, error: "", data: null };
  const request = new AbortController();
  selectionControllers.set(run, request);
  try {
    const data = await api<ContextSelectionView>(
      `/runs/${encodeURIComponent(run)}/context-selection`,
      { signal: request.signal },
    );
    if (!disposed) selections[run] = { loading: false, error: "", data };
  } catch (caught) {
    if (disposed) return;
    if (caught instanceof ApiError && caught.status === 401)
      emit("unauthorized");
    selections[run] = {
      loading: false,
      data: null,
      error:
        caught instanceof ApiError && caught.status === 404
          ? "没有可访问的上下文选择记录，来源可能已失效。"
          : errorMessage(caught),
    };
  } finally {
    selectionControllers.delete(run);
  }
}
async function load(append = false, apply = false) {
  if (loading.value || disposed) return;
  if (apply) applied.value = { ...filters };
  loading.value = true;
  error.value = "";
  controller = new AbortController();
  const requestEpoch = ++epoch;
  const query = new URLSearchParams({ limit: "50" });
  for (const [key, value] of Object.entries(applied.value))
    if (value.trim()) query.set(key, value.trim());
  if (append && nextCursor.value) query.set("cursor", nextCursor.value);
  try {
    const result = await api<LogPage>(`/runtime-logs?${query}`, {
      signal: controller.signal,
    });
    if (disposed || requestEpoch !== epoch) return;
    snapshot.value = result;
    items.value = append
      ? [
          ...items.value,
          ...result.items.filter(
            (item) => !items.value.some((old) => old.id === item.id),
          ),
        ]
      : result.items;
    nextCursor.value = result.next_cursor;
    loadedAt.value = Date.now();
    if (!append) expanded.value = new Set();
  } catch (caught) {
    if (disposed || requestEpoch !== epoch) return;
    if (caught instanceof ApiError && caught.status === 401) {
      autoRefresh.value = false;
      emit("unauthorized");
    }
    error.value = errorMessage(caught);
  } finally {
    if (!disposed && requestEpoch === epoch) loading.value = false;
  }
}
function schedule() {
  timer = setTimeout(async () => {
    if (
      !disposed &&
      autoRefresh.value &&
      !document.hidden &&
      !hasUnapplied.value &&
      !expanded.value.size
    )
      await load();
    if (!disposed) schedule();
  }, 5000);
}
function reset() {
  Object.assign(filters, { kind: "", status: "", run_id: "", q: "" });
  void load(false, true);
}
onMounted(() => {
  void load();
  schedule();
});
onUnmounted(() => {
  disposed = true;
  epoch++;
  controller?.abort();
  for (const request of selectionControllers.values()) request.abort();
  selectionControllers.clear();
  clearTimeout(timer);
});
</script>

<template>
  <section class="runtime-logs" aria-label="运行日志">
    <div class="panel runtime-log-toolbar">
      <div class="panel-heading">
        <div>
          <h2>运行日志</h2>
          <p>按时间查看运行过程，展开记录核对输入、输出与调用详情。</p>
        </div>
        <button
          class="button secondary small"
          :disabled="loading"
          @click="load()"
        >
          刷新日志
        </button>
      </div>
      <form class="runtime-log-filters" @submit.prevent="load(false, true)">
        <label class="field"
          ><span>记录类型</span
          ><AdminSelect
            v-model="filters.kind"
            aria-label="记录类型"
            :options="[
              { value: '', label: '全部类型' },
              ...Object.entries(kindLabels).map(([value, label]) => ({
                value,
                label,
              })),
            ]"
        /></label>
        <label class="field"
          ><span>运行状态</span
          ><AdminSelect
            v-model="filters.status"
            aria-label="运行状态"
            :options="[
              { value: '', label: '全部状态' },
              ...Object.entries(statusLabels).map(([value, label]) => ({
                value,
                label,
              })),
            ]"
        /></label>
        <label class="field"
          ><span>Run ID</span
          ><input
            v-model="filters.run_id"
            placeholder="按运行编号筛选"
            maxlength="128"
        /></label>
        <label class="field"
          ><span>搜索内容</span
          ><input
            v-model="filters.q"
            placeholder="标题或日志内容"
            maxlength="200"
        /></label>
        <div class="runtime-actions">
          <button
            type="submit"
            class="button primary small"
            :disabled="loading"
          >
            筛选日志</button
          ><button
            type="button"
            class="button secondary small"
            :disabled="loading"
            @click="reset"
          >
            清除筛选
          </button>
        </div>
      </form>
      <div class="runtime-log-context">
        <label
          ><input v-model="autoRefresh" type="checkbox" /> 每 5 秒刷新</label
        ><span v-if="autoRefresh && expanded.size">展开详情时暂停自动刷新</span
        ><span v-if="hasUnapplied">筛选条件待应用</span
        ><span v-if="loadedAt">更新于 {{ formatDate(loadedAt) }}</span>
      </div>
      <p v-if="snapshot" class="footnote">
        日志保留 {{ snapshot.retention.days }} 天，最多
        {{ snapshot.retention.max_events.toLocaleString() }} 条。{{
          !snapshot.capabilities.asr ? "语音识别尚未接入。" : ""
        }}{{
          !snapshot.capabilities.subagent ? "协作分工尚未接入。" : ""
        }}未接入能力不产生执行记录。
      </p>
    </div>
    <p v-if="error" class="runtime-error panel" role="alert">{{ error }}</p>
    <div class="panel runtime-log-list" :aria-busy="loading">
      <div class="runtime-log-list-heading">
        <h2>运行记录</h2>
        <span class="badge neutral">已载入 {{ items.length }} 条</span>
      </div>
      <p v-if="!items.length" class="runtime-log-empty" role="status">
        {{ loading ? "正在载入运行日志…" : "暂无符合条件的运行记录。" }}
      </p>
      <article
        v-for="item in items"
        :key="item.id"
        class="runtime-log-entry"
        :class="{
          'runtime-log-failed': [
            'failed',
            'interrupted',
            'outcome_unknown',
          ].includes(item.status),
        }"
      >
        <button
          type="button"
          class="runtime-log-summary"
          :aria-expanded="expanded.has(item.id)"
          :aria-controls="`runtime-event-${item.id}`"
          @click="toggle(item.id)"
        >
          <span class="runtime-log-type">{{
            kindLabels[item.kind] || item.kind
          }}</span>
          <span class="runtime-log-caption"
            ><strong>{{ item.title }}</strong
            ><small>{{
              item.content ? item.content.slice(0, 160) : "展开查看记录详情"
            }}</small></span
          >
          <span class="runtime-log-meta"
            ><time>{{ formatDate(item.created_at) }}</time
            ><span
              >{{ item.source }} ·
              {{ statusLabels[item.status] || item.status }}</span
            ></span
          >
          <span class="runtime-log-chevron" aria-hidden="true">{{
            expanded.has(item.id) ? "−" : "+"
          }}</span>
        </button>
        <div
          v-if="expanded.has(item.id)"
          :id="`runtime-event-${item.id}`"
          class="runtime-log-detail"
        >
          <dl>
            <div>
              <dt>事件编号</dt>
              <dd>#{{ item.id }}</dd>
            </div>
            <div v-if="item.run_id">
              <dt>Run ID</dt>
              <dd>{{ item.run_id }}</dd>
            </div>
            <div v-if="item.conversation_id">
              <dt>会话编号</dt>
              <dd>{{ item.conversation_id }}</dd>
            </div>
          </dl>
          <pre v-if="item.content">{{ item.content }}</pre>
          <section
            v-if="item.run_id"
            class="runtime-context-evidence"
            aria-label="上下文选择依据"
          >
            <h3>上下文选择依据</h3>
            <button
              type="button"
              :disabled="selections[item.run_id]?.loading"
              @click="loadSelection(item.run_id)"
            >
              {{
                selections[item.run_id]?.loading
                  ? "正在读取依据…"
                  : "读取上下文选择记录"
              }}
            </button>
            <p v-if="selections[item.run_id]?.error" role="alert">
              {{ selections[item.run_id]?.error }}
            </p>
            <p v-if="selections[item.run_id]?.data?.available === false">
              该运行没有预算选择记录，历史窗口与选择过程保持未知。
            </p>
            <template v-if="selections[item.run_id]?.data?.selection">
              <p>
                {{
                  selections[item.run_id]!.data!.selection!.accepted
                    ? "预算校验通过"
                    : "预算拒绝，请求未发出"
                }}
                · 策略 v{{
                  selections[item.run_id]!.data!.selection!.policy.revision
                }}
                ·
                {{
                  selections[item.run_id]!.data!.selection!.serialized_bytes
                }}
                字节
              </p>
              <p>
                保守估计输入
                {{
                  selections[item.run_id]!.data!.selection!
                    .estimated_input_tokens
                }}，响应预留
                {{
                  selections[item.run_id]!.data!.selection!.policy
                    .response_reserve_tokens
                }}；本次请求输出上限
                {{
                  selections[item.run_id]!.data!.selection!
                    .actual_response_tokens
                }}。这是估计，不是精确 token 计费。
              </p>
              <p
                v-if="
                  selections[item.run_id]!.data!.selection!.rejection_reason
                "
              >
                {{
                  selectionReasonLabels[
                    selections[item.run_id]!.data!.selection!.rejection_reason!
                  ] ||
                  selections[item.run_id]!.data!.selection!.rejection_reason
                }}
              </p>
              <ul class="runtime-context-blocks">
                <li
                  v-for="block in selections[item.run_id]!.data!.selection!
                    .blocks"
                  :key="block.id"
                >
                  <strong
                    >{{ contextBlockLabels[block.kind] || block.kind }} ·
                    {{ block.selected ? "已选入" : "已省略" }}</strong
                  ><span>{{
                    selectionReasonLabels[block.reason] || block.reason
                  }}</span
                  ><small
                    v-for="source in block.source_refs"
                    :key="`${source.kind}:${source.id}:${source.revision}`"
                    >来源 {{ source.kind }} · {{ source.id }} · v{{
                      source.revision
                    }}<template
                      v-if="source.epoch !== null && source.epoch !== undefined"
                    >
                      · 水位 {{ source.epoch }}</template
                    ></small
                  >
                </li>
              </ul>
            </template>
          </section>
          <details v-if="Object.keys(item.details).length">
            <summary>结构化详情</summary>
            <pre>{{ details(item) }}</pre>
          </details>
        </div>
      </article>
      <div v-if="nextCursor" class="runtime-log-more">
        <button
          class="button secondary"
          :disabled="loading"
          @click="load(true)"
        >
          {{ loading ? "载入中…" : "加载更早记录" }}
        </button>
      </div>
    </div>
  </section>
</template>

<style scoped>
.runtime-context-evidence {
  border-top: 1px solid var(--line);
  padding-top: 16px;
  margin-top: 16px;
  font-size: 12px;
  line-height: 1.8;
}
.runtime-context-evidence h3 {
  margin: 0 0 12px;
}
.runtime-context-blocks {
  list-style: none;
  padding: 0;
  display: grid;
  gap: 10px;
}
.runtime-context-blocks li {
  display: grid;
  gap: 4px;
  padding: 10px;
  border: 1px solid var(--line);
  border-radius: 8px;
  overflow-wrap: anywhere;
}
.runtime-context-blocks span,
.runtime-context-blocks small {
  color: var(--muted);
}
</style>
