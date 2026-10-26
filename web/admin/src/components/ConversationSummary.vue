<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, useId, watch } from "vue";
import { api, ApiError, errorMessage } from "../api";
import { formatDate } from "../validation";
import AdminSelect from "./AdminSelect.vue";
import type { Conversation, Timestamp } from "../types";

interface SummaryResponse {
  conversation_id: string;
  revision: string;
  summary: null | {
    content: string;
    revision: number;
    source_count: number;
    redacted_source_count?: number;
    truncated_source_count?: number;
    through_run_rowid: number;
    epoch: number;
    source_run_ids: string[];
    source_digest: string;
  };
  last_job: null | {
    status: string;
    created_at: Timestamp;
    error_code?: string | null;
  };
  policy: {
    keep_recent_turns: number;
    max_batch_turns: number;
    max_source_bytes: number;
    max_summary_bytes: number;
    timeout_seconds: number;
  };
}

const fieldId = useId();
const conversations = ref<Conversation[]>([]);
const selected = ref("");
const view = ref<SummaryResponse | null>(null);
const loadingConversations = ref(true);
const loading = ref(false);
const rebuilding = ref(false);
const confirming = ref(false);
const conflict = ref(false);
const error = ref("");
const notice = ref("");
let sourceController: AbortController | null = null;
let requestController: AbortController | null = null;
let mutationController: AbortController | null = null;
let requestEpoch = 0;
let disposed = false;

const selectedTitle = computed(
  () =>
    conversations.value.find((item) => item.id === selected.value)?.title ||
    selected.value,
);
const jobLabel = computed(() => {
  switch (view.value?.last_job?.status) {
    case "completed":
      return "上次整理已完成";
    case "running":
      return "正在整理";
    case "failed":
    case "timed_out":
    case "timeout":
      return "上次整理未完成，沿用原窗口";
    case "cancelled":
      return "上次整理已取消，沿用原窗口";
    case "interrupted":
      return "上次整理已中断，沿用原窗口";
    case "stale":
      return "上次结果已失效，未写入摘要";
    case "pending_next_run":
      return "等待下次普通消息重建";
    default:
      return view.value?.last_job ? "上次整理状态" : "尚无整理任务";
  }
});

function summaryPath(id: string) {
  return `/conversations/${encodeURIComponent(id)}/summary`;
}
async function loadConversations() {
  const controller = new AbortController();
  sourceController = controller;
  const deadline = setTimeout(() => controller.abort(), 10_000);
  try {
    const result = await api<{ items: Conversation[] }>("/conversations", {
      signal: controller.signal,
    });
    if (disposed) return;
    conversations.value = result.items.filter(
      (item) => !item.test && !item.is_test && !item.character?.is_test,
    );
  } catch (caught) {
    if (!disposed)
      error.value = `可查看的会话未能载入。${errorMessage(caught)}`;
  } finally {
    clearTimeout(deadline);
    if (!disposed) loadingConversations.value = false;
  }
}
async function loadSummary() {
  const id = selected.value;
  if (!id) return;
  requestController?.abort();
  const epoch = ++requestEpoch;
  const controller = new AbortController();
  requestController = controller;
  const deadline = setTimeout(() => controller.abort(), 10_000);
  loading.value = true;
  error.value = "";
  try {
    const result = await api<SummaryResponse>(summaryPath(id), {
      signal: controller.signal,
    });
    if (
      disposed ||
      epoch !== requestEpoch ||
      selected.value !== id ||
      controller.signal.aborted
    )
      return;
    if (result.conversation_id !== id || typeof result.revision !== "string") {
      error.value = "摘要响应与所选会话或版本格式不一致，请刷新后重试。";
      return;
    }
    view.value = result;
    conflict.value = false;
  } catch (caught) {
    if (!disposed && epoch === requestEpoch && selected.value === id)
      error.value = `摘要未能更新${view.value ? "，以下保留上次读取的内容" : ""}。${errorMessage(caught)}`;
  } finally {
    clearTimeout(deadline);
    if (epoch === requestEpoch && !disposed) loading.value = false;
  }
}
function askRebuild() {
  if (!view.value || loading.value || rebuilding.value || conflict.value)
    return;
  confirming.value = true;
  error.value = "";
  notice.value = "";
}
async function rebuild() {
  const current = view.value;
  const id = selected.value;
  if (
    !current ||
    !confirming.value ||
    rebuilding.value ||
    current.conversation_id !== id
  )
    return;
  const controller = new AbortController();
  mutationController = controller;
  const deadline = setTimeout(() => controller.abort(), 10_000);
  rebuilding.value = true;
  error.value = "";
  notice.value = "";
  try {
    const result = await api<{ status: string; revision?: string }>(
      `${summaryPath(id)}/rebuild`,
      {
        method: "POST",
        body: {},
        revision: current.revision,
        signal: controller.signal,
      },
    );
    if (disposed || selected.value !== id) return;
    confirming.value = false;
    if (result.status !== "pending_next_run") {
      error.value = "重建请求返回了未知状态，请刷新确认实际结果。";
      conflict.value = true;
      return;
    }
    view.value = null;
    notice.value = "当前派生摘要已清除。下次普通消息时重建；原始消息保留。";
    await loadSummary();
  } catch (caught) {
    if (disposed || selected.value !== id) return;
    if (caught instanceof ApiError && caught.status === 409) {
      confirming.value = false;
      conflict.value = true;
      error.value =
        "摘要已在别处更新，尚未清除。当前展示已保留，请刷新摘要后重新核对。";
    } else {
      confirming.value = false;
      conflict.value = true;
      error.value = `未能确认重建结果，请先刷新摘要再重试。${errorMessage(caught)}`;
    }
  } finally {
    clearTimeout(deadline);
    mutationController = null;
    if (!disposed) rebuilding.value = false;
  }
}

watch(selected, () => {
  requestEpoch++;
  requestController?.abort();
  view.value = null;
  confirming.value = false;
  conflict.value = false;
  error.value = "";
  notice.value = "";
  loading.value = false;
  if (selected.value) void loadSummary();
});
onMounted(() => void loadConversations());
onUnmounted(() => {
  disposed = true;
  requestEpoch++;
  sourceController?.abort();
  requestController?.abort();
  mutationController?.abort();
});
</script>

<template>
  <section class="panel summary-panel" aria-label="会话摘要">
    <div class="panel-heading summary-heading">
      <div>
        <h2>长会话摘要</h2>
        <p>仅用于所选来源会话，不共享到其他会话或群聊。</p>
      </div>
      <button
        type="button"
        class="button secondary small"
        :disabled="!selected || loading || rebuilding"
        @click="loadSummary"
      >
        刷新摘要
      </button>
    </div>
    <p class="summary-policy-note">
      普通对话达到阈值时自动整理；每次普通任务最多整理一次、最长 20
      秒。失败时沿用原窗口。摘要是可引用的资料，不是系统指令。
    </p>
    <label class="field"
      ><span>查看摘要的会话</span
      ><AdminSelect
        v-model="selected"
        aria-label="查看摘要的会话"
        :aria-describedby="`${fieldId}-source-help`"
        :disabled="loadingConversations || rebuilding"
        :options="[
          {
            value: '',
            label: loadingConversations ? '正在读取会话…' : '请选择普通会话',
          },
          ...conversations.map((conversation) => ({
            value: conversation.id,
            label: conversation.title,
          })),
        ]"
      /><small :id="`${fieldId}-source-help`"
        >仅列出当前已认证账户可访问的普通来源会话；草稿测试会话不参与整理。</small
      ></label
    >
    <div v-if="error" role="alert" class="alert error">
      <span>{{ error }}</span>
    </div>
    <div v-if="notice" role="status" class="alert success">
      <span>{{ notice }}</span>
    </div>
    <p v-if="!loadingConversations && !conversations.length" class="footnote">
      暂无可查看的普通会话。先在对话工作台建立普通会话，测试草稿不会出现在这里。
    </p>
    <p v-if="loading" role="status" class="muted summary-loading">
      正在读取所选会话摘要…
    </p>
    <template v-if="view">
      <div class="summary-current-heading">
        <strong>{{ selectedTitle }}</strong
        ><span class="badge neutral">{{
          view.summary ? `摘要 v${view.summary.revision}` : "暂无摘要"
        }}</span>
      </div>
      <article
        v-if="view.summary"
        class="summary-content-box"
        aria-label="当前派生摘要"
      >
        <p class="summary-content">{{ view.summary.content }}</p>
        <p class="footnote">
          来源 {{ view.summary.source_count }} 轮 · 仅供本会话引用
        </p>
        <p
          v-if="
            (view.summary.redacted_source_count ?? 0) > 0 ||
            (view.summary.truncated_source_count ?? 0) > 0
          "
          class="footnote summary-source-limits"
        >
          <span v-if="(view.summary.redacted_source_count ?? 0) > 0"
            >已省略敏感来源 {{ view.summary.redacted_source_count }} 轮。</span
          >
          <span v-if="(view.summary.truncated_source_count ?? 0) > 0"
            >超长来源摘录 {{ view.summary.truncated_source_count }} 轮。</span
          >
          摘要不代表完整原文，原始消息独立保留。
        </p>
      </article>
      <p v-else class="summary-empty">
        此会话尚无派生摘要。普通对话达到整理阈值后会自动生成；原始消息仍独立保存。
      </p>
      <div class="summary-job">
        <span>{{ jobLabel }}</span
        ><time v-if="view.last_job">{{
          formatDate(view.last_job.created_at)
        }}</time
        ><code v-if="view.last_job?.error_code">{{
          view.last_job.error_code
        }}</code>
      </div>
      <details v-if="view.summary" class="summary-provenance">
        <summary>查看摘要来源</summary>
        <dl>
          <div>
            <dt>覆盖至任务水位</dt>
            <dd>{{ view.summary.through_run_rowid }}</dd>
          </div>
          <div>
            <dt>失效代次</dt>
            <dd>{{ view.summary.epoch }}</dd>
          </div>
          <div>
            <dt>来源摘要</dt>
            <dd>
              <code>{{ view.summary.source_digest }}</code>
            </dd>
          </div>
          <div>
            <dt>来源任务</dt>
            <dd>
              <ul>
                <li v-for="id in view.summary.source_run_ids" :key="id">
                  <code>{{ id }}</code>
                </li>
              </ul>
            </dd>
          </div>
        </dl>
      </details>
      <p class="footnote summary-budget">
        保留最近 {{ view.policy.keep_recent_turns }} 轮；每次最多整理
        {{ view.policy.max_batch_turns }} 轮、{{
          view.policy.max_source_bytes.toLocaleString()
        }}
        字节来源，摘要最多
        {{ view.policy.max_summary_bytes.toLocaleString() }} 字节，最长
        {{ view.policy.timeout_seconds }} 秒。
      </p>
      <button
        type="button"
        class="button secondary small"
        :disabled="loading || rebuilding || conflict"
        @click="askRebuild"
      >
        重建摘要
      </button>
      <section
        v-if="confirming"
        class="summary-confirm"
        role="region"
        aria-label="确认重建摘要"
      >
        <h3>重建“{{ selectedTitle }}”的派生摘要？</h3>
        <p>
          清除当前派生摘要，下次普通消息时重建，原始消息保留。此操作不会遗忘原始消息，也不会清除记忆库中的独立记忆。
        </p>
        <p v-if="view.summary" class="summary-content">
          {{ view.summary.content }}
        </p>
        <div class="summary-actions">
          <button
            type="button"
            class="button primary small"
            :disabled="loading || rebuilding"
            @click="rebuild"
          >
            确认清除派生摘要并安排重建</button
          ><button
            type="button"
            class="button secondary small"
            :disabled="rebuilding"
            @click="confirming = false"
          >
            取消重建
          </button>
        </div>
      </section>
    </template>
  </section>
</template>

<style scoped>
.summary-panel,
.summary-panel > *,
.summary-panel .field {
  min-width: 0;
}
.summary-heading {
  flex-wrap: wrap;
}
.summary-heading > div {
  min-width: 0;
  flex: 1 1 220px;
}
.summary-policy-note {
  font-size: 12px;
  line-height: 1.9;
  color: var(--muted);
  background: var(--soft-blue);
  border-radius: 8px;
  padding: 12px 14px;
  margin-bottom: 20px;
}
.summary-panel select {
  width: 100%;
  min-width: 0;
  max-width: 100%;
}
.summary-current-heading,
.summary-job,
.summary-actions {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 10px;
}
.summary-current-heading {
  justify-content: space-between;
  margin: 18px 0 12px;
}
.summary-current-heading strong {
  min-width: 0;
  overflow-wrap: anywhere;
  font-size: 13px;
}
.summary-content-box {
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 14px 16px;
}
.summary-content {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  font-size: 13px;
  line-height: 1.9;
}
.summary-content-box .footnote {
  margin-top: 12px;
}
.summary-empty,
.summary-loading,
.summary-job {
  font-size: 12px;
  color: var(--muted);
  line-height: 1.9;
  margin: 12px 0;
}
.summary-job code,
.summary-provenance code {
  overflow-wrap: anywhere;
  word-break: break-word;
}
.summary-provenance {
  font-size: 12px;
  color: var(--muted);
  margin: 16px 0;
}
.summary-provenance summary {
  cursor: pointer;
  color: var(--indigo);
}
.summary-provenance dl {
  margin-top: 12px;
}
.summary-provenance dl > div {
  margin: 8px 0;
}
.summary-provenance dt {
  font-weight: 600;
}
.summary-provenance dd {
  min-width: 0;
  margin: 3px 0 0;
  overflow-wrap: anywhere;
}
.summary-provenance ul {
  padding-left: 18px;
}
.summary-budget {
  margin: 16px 0;
}
.summary-confirm {
  padding: 16px;
  border: 1px solid #e4d2b1;
  border-radius: 9px;
  background: #fdf9f1;
  margin-top: 16px;
}
.summary-confirm h3 {
  font-size: 14px;
  overflow-wrap: anywhere;
}
.summary-confirm > p {
  font-size: 12px;
  line-height: 1.9;
  margin: 12px 0;
}
.summary-confirm button {
  white-space: normal;
  text-align: left;
  max-width: 100%;
}
@media (max-width: 480px) {
  .summary-content-box,
  .summary-confirm {
    padding: 12px;
  }
  .summary-actions {
    align-items: stretch;
    flex-direction: column;
  }
}
</style>
