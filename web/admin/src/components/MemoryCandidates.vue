<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, useId, watch } from "vue";
import { api, ApiError, errorMessage } from "../api";
import { formatDate } from "../validation";
import type { Conversation, Timestamp } from "../types";

type Revision = string | number;
type JobStatus =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "expired";
type CandidateStatus =
  "pending" | "accepted" | "rejected" | "expired" | "invalidated";
interface Job {
  id: string;
  conversation_id: string;
  status: JobStatus;
  revision: Revision;
  created_at: Timestamp;
  expires_at: Timestamp;
  completed_at?: Timestamp | null;
  error_code?: string | null;
  source_count: number;
  redacted_source_count: number;
}
interface Candidate {
  id: string;
  job_id: string;
  status: CandidateStatus;
  revision: Revision;
  content?: string;
  source_message_ids: string[];
  expires_at: Timestamp;
  memory_id?: string;
}
interface Source {
  id: string;
  content: string;
  created_at: Timestamp;
  byte_length: number;
}
interface Snapshot {
  source_revision: string;
  job: Job | null;
  items: Candidate[];
  sources: Source[];
  policy: {
    max_sources: number;
    max_source_bytes: number;
    max_candidates: number;
    max_content_bytes: number;
  };
}

const fieldId = useId();
const emit = defineEmits<{ accepted: [] }>();
const conversations = ref<Conversation[]>([]);
const selected = ref("");
const snapshot = ref<Snapshot | null>(null);
const sourceIds = ref<string[]>([]);
const loadingConversations = ref(true);
const loading = ref(false);
const mutating = ref(false);
const conflict = ref(false);
const sessionExpired = ref(false);
const confirmCandidateId = ref("");
const confirmCancel = ref(false);
const error = ref("");
const notice = ref("");
const controllers = new Set<AbortController>();
let jobController: AbortController | null = null;
let snapshotController: AbortController | null = null;
let pollTimer: ReturnType<typeof setTimeout> | undefined;
let expiryTimer: ReturnType<typeof setTimeout> | undefined;
let epoch = 0;
let pollCount = 0;
let disposed = false;

const title = computed(
  () =>
    conversations.value.find((item) => item.id === selected.value)?.title ||
    selected.value,
);
const job = computed(() => snapshot.value?.job ?? null);
const activeJob = computed(
  () => job.value?.status === "queued" || job.value?.status === "running",
);
const policy = computed(() => ({
  max_sources: Math.min(12, snapshot.value?.policy.max_sources ?? 12),
  max_source_bytes: Math.min(
    12000,
    snapshot.value?.policy.max_source_bytes ?? 12000,
  ),
  max_candidates: Math.min(3, snapshot.value?.policy.max_candidates ?? 3),
  max_content_bytes: Math.min(
    1000,
    snapshot.value?.policy.max_content_bytes ?? 1000,
  ),
}));
const chosenSources = computed(() =>
  (snapshot.value?.sources ?? []).filter((source) =>
    sourceIds.value.includes(source.id),
  ),
);
const selectedBytes = computed(() =>
  chosenSources.value.reduce((sum, source) => sum + bytes(source.content), 0),
);
const selectionError = computed(() => {
  if (!sourceIds.value.length) return "请明确勾选至少一条来源消息。";
  if (sourceIds.value.length > policy.value.max_sources)
    return `最多选择 ${policy.value.max_sources} 条来源消息。`;
  if (chosenSources.value.length !== sourceIds.value.length)
    return "来源已变化，请刷新后重新选择。";
  if (selectedBytes.value > policy.value.max_source_bytes)
    return `所选来源不能超过 ${policy.value.max_source_bytes.toLocaleString()} UTF-8 字节。`;
  return "";
});
const confirmCandidate = computed(
  () =>
    snapshot.value?.items.find(
      (item) =>
        item.id === confirmCandidateId.value && item.status === "pending",
    ) ?? null,
);
const jobText: Record<JobStatus, string> = {
  queued: "等待提取",
  running: "正在提取",
  completed: "提取已完成，候选仍需审核",
  failed: "提取失败，未自动重试",
  cancelled: "提取已取消",
  interrupted: "提取已中断，未自动重试",
  expired: "提取任务已到期",
};
const statusText: Record<CandidateStatus, string> = {
  pending: "待审核，尚未记住",
  accepted: "已接受为来源账号域私有记忆",
  rejected: "已拒绝",
  expired: "已到期",
  invalidated: "来源已变化，候选失效",
};

function bytes(content: string) {
  return new TextEncoder().encode(content).length;
}
function timestamp(value: Timestamp) {
  return typeof value === "number"
    ? value < 1e12
      ? value * 1000
      : value
    : Date.parse(value);
}
function current(token: number, id: string) {
  return (
    !disposed &&
    !sessionExpired.value &&
    epoch === token &&
    selected.value === id
  );
}
function request() {
  const controller = new AbortController();
  controllers.add(controller);
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, 10000);
  return {
    controller,
    timedOut: () => timedOut,
    done: () => {
      clearTimeout(timer);
      controllers.delete(controller);
    },
  };
}
function stopPolling() {
  clearTimeout(pollTimer);
  pollTimer = undefined;
  jobController?.abort();
  jobController = null;
}
function clearSensitive() {
  epoch++;
  stopPolling();
  clearTimeout(expiryTimer);
  for (const controller of controllers) controller.abort();
  controllers.clear();
  snapshot.value = null;
  sourceIds.value = [];
  confirmCandidateId.value = "";
  confirmCancel.value = false;
}
function failure(caught: unknown, fallback: string) {
  if (caught instanceof ApiError && caught.status === 401) {
    clearSensitive();
    conversations.value = [];
    sessionExpired.value = true;
    selected.value = "";
    loading.value = false;
    mutating.value = false;
    error.value = "登录已过期，来源正文和候选已清除。请重新登录。";
    return;
  }
  stopPolling();
  if (caught instanceof ApiError && caught.status === 409) {
    conflict.value = true;
    confirmCandidateId.value = "";
    confirmCancel.value = false;
    error.value =
      "来源或候选状态已更新，未重复执行操作。当前选择和展示已保留，请刷新候选后重新核对。";
  } else {
    error.value = `${fallback}${errorMessage(caught)}`;
  }
}
function sourceFor(id: string) {
  return snapshot.value?.sources.find((source) => source.id === id);
}
function reviewable(item: Candidate) {
  return (
    item.status === "pending" &&
    typeof item.content === "string" &&
    !!item.content.trim() &&
    bytes(item.content) <= policy.value.max_content_bytes &&
    item.source_message_ids.length > 0 &&
    item.source_message_ids.every((id) => !!sourceFor(id)) &&
    timestamp(item.expires_at) > Date.now()
  );
}
function scrubExpired() {
  if (!snapshot.value) return;
  const now = Date.now();
  snapshot.value.items = snapshot.value.items.map((item) =>
    item.status === "pending" && timestamp(item.expires_at) <= now
      ? { ...item, status: "expired", content: undefined }
      : item,
  );
  if (activeJob.value && job.value && timestamp(job.value.expires_at) <= now) {
    snapshot.value.job = { ...job.value, status: "expired" };
    stopPolling();
  }
  if (!confirmCandidate.value) confirmCandidateId.value = "";
}
function armExpiry() {
  clearTimeout(expiryTimer);
  scrubExpired();
  const deadlines = (snapshot.value?.items ?? [])
    .filter((item) => item.status === "pending")
    .map((item) => timestamp(item.expires_at));
  if (activeJob.value && job.value)
    deadlines.push(timestamp(job.value.expires_at));
  const nearest = Math.min(
    ...deadlines.filter(
      (value) => Number.isFinite(value) && value > Date.now(),
    ),
  );
  if (Number.isFinite(nearest))
    expiryTimer = setTimeout(
      armExpiry,
      Math.min(2147483647, Math.max(1, nearest - Date.now())),
    );
}
function schedulePoll() {
  clearTimeout(pollTimer);
  if (!activeJob.value || conflict.value || sessionExpired.value || disposed)
    return;
  if (pollCount >= 60) {
    error.value =
      "已达到本次状态查询上限，轮询已暂停。任务结果尚待确认，请手动刷新候选。";
    return;
  }
  pollTimer = setTimeout(() => void pollJob(), 2000);
}
async function loadConversations() {
  const pending = request();
  try {
    const result = await api<{ items: Conversation[] }>("/conversations", {
      signal: pending.controller.signal,
    });
    if (disposed || sessionExpired.value || pending.controller.signal.aborted)
      return;
    conversations.value = result.items.filter(
      (item) => !item.test && !item.is_test && !item.character?.is_test,
    );
  } catch (caught) {
    if (!disposed) failure(caught, "来源会话未能载入。");
  } finally {
    pending.done();
    if (!disposed) loadingConversations.value = false;
  }
}
async function loadSnapshot() {
  if (!selected.value || sessionExpired.value || disposed) return;
  stopPolling();
  snapshotController?.abort();
  const id = selected.value;
  const token = epoch;
  const pending = request();
  snapshotController = pending.controller;
  loading.value = true;
  error.value = "";
  try {
    const result = await api<Snapshot>(
      `/conversations/${encodeURIComponent(id)}/memory-candidates`,
      { signal: pending.controller.signal },
    );
    if (!current(token, id) || pending.controller.signal.aborted) return;
    if (
      typeof result.source_revision !== "string" ||
      !Array.isArray(result.sources) ||
      !Array.isArray(result.items) ||
      (result.job && result.job.conversation_id !== id)
    )
      throw new Error("invalid candidate response");
    result.items = result.items.map((item) =>
      item.status === "pending" ? item : { ...item, content: undefined },
    );
    snapshot.value = result;
    sourceIds.value = sourceIds.value.filter((value) =>
      result.sources.some((source) => source.id === value),
    );
    confirmCandidateId.value = "";
    confirmCancel.value = false;
    conflict.value = false;
    pollCount = 0;
    armExpiry();
    if (!mutating.value) schedulePoll();
  } catch (caught) {
    if (
      current(token, id) &&
      (!pending.controller.signal.aborted || pending.timedOut())
    )
      failure(caught, "候选未能更新，已保留上次展示。请手动刷新。");
  } finally {
    pending.done();
    if (current(token, id) && snapshotController === pending.controller)
      loading.value = false;
  }
}
async function pollJob() {
  pollTimer = undefined;
  const target = job.value;
  if (
    !target ||
    !activeJob.value ||
    conflict.value ||
    disposed ||
    sessionExpired.value
  )
    return;
  if (loading.value || mutating.value) {
    schedulePoll();
    return;
  }
  const id = selected.value;
  const token = epoch;
  const pending = request();
  jobController = pending.controller;
  pollCount++;
  try {
    const result = await api<Job>(
      `/memory-candidate-jobs/${encodeURIComponent(target.id)}`,
      { signal: pending.controller.signal },
    );
    if (
      !current(token, id) ||
      pending.controller.signal.aborted ||
      job.value?.id !== target.id
    )
      return;
    if (result.id !== target.id || result.conversation_id !== id)
      throw new Error("mismatched job response");
    snapshot.value!.job = result;
    armExpiry();
    if (activeJob.value) schedulePoll();
    else await loadSnapshot();
  } catch (caught) {
    if (
      current(token, id) &&
      (!pending.controller.signal.aborted || pending.timedOut())
    )
      failure(caught, "状态查询已停止，请手动刷新候选。");
  } finally {
    pending.done();
    if (jobController === pending.controller) jobController = null;
  }
}
async function mutate(action: (signal: AbortSignal) => Promise<void>) {
  if (mutating.value || loading.value || conflict.value || sessionExpired.value)
    return;
  const id = selected.value;
  const token = epoch;
  const pending = request();
  stopPolling();
  mutating.value = true;
  error.value = "";
  notice.value = "";
  try {
    await action(pending.controller.signal);
  } catch (caught) {
    if (current(token, id)) {
      if (!(
        caught instanceof ApiError &&
        (caught.status === 401 || caught.status === 409)
      ))
        conflict.value = true;
      failure(caught, "未能确认操作结果，不会自动重试。请刷新候选。");
    }
  } finally {
    pending.done();
    if (current(token, id)) {
      mutating.value = false;
      if (!conflict.value) schedulePoll();
    }
  }
}
async function extract() {
  if (!snapshot.value || activeJob.value) return;
  if (selectionError.value) {
    error.value = selectionError.value;
    return;
  }
  const id = selected.value;
  const token = epoch;
  const revision = snapshot.value.source_revision;
  const ids = [...sourceIds.value];
  await mutate(async (signal) => {
    const result = await api<Job>(
      `/conversations/${encodeURIComponent(id)}/memory-candidates/extract`,
      {
        method: "POST",
        body: { source_message_ids: ids },
        etag: revision,
        signal,
      },
    );
    if (!current(token, id) || signal.aborted) return;
    if (result.conversation_id !== id)
      throw new Error("mismatched extraction response");
    snapshot.value!.job = result;
    sourceIds.value = [];
    pollCount = 0;
    notice.value = "已提交一次提取。模型候选尚未写入记忆，必须逐条审核接受。";
    armExpiry();
    if (!activeJob.value) await loadSnapshot();
  });
}
async function decide(item: Candidate, decision: "accept" | "reject") {
  if (
    item.status !== "pending" ||
    (decision === "accept" &&
      (confirmCandidateId.value !== item.id || !reviewable(item)))
  )
    return;
  const id = selected.value;
  const token = epoch;
  await mutate(async (signal) => {
    await api<unknown>(
      `/memory-candidates/${encodeURIComponent(item.id)}/${decision}`,
      { method: "POST", body: {}, etag: String(item.revision), signal },
    );
    if (!current(token, id) || signal.aborted) return;
    snapshot.value!.items = snapshot.value!.items.map((entry) =>
      entry.id === item.id
        ? {
            ...entry,
            status: decision === "accept" ? "accepted" : "rejected",
            content: undefined,
          }
        : entry,
    );
    confirmCandidateId.value = "";
    notice.value =
      decision === "accept"
        ? "已接受为一条来源账号域私有记忆；没有发布为共享记忆，也没有覆盖已有事实。"
        : "候选已拒绝，未写入记忆。";
    if (decision === "accept") emit("accepted");
    await loadSnapshot();
  });
}
async function cancelJob() {
  const target = job.value;
  if (!target || !activeJob.value || !confirmCancel.value) return;
  const id = selected.value;
  const token = epoch;
  await mutate(async (signal) => {
    await api<unknown>(
      `/memory-candidate-jobs/${encodeURIComponent(target.id)}`,
      { method: "DELETE", etag: String(target.revision), signal },
    );
    if (!current(token, id) || signal.aborted) return;
    snapshot.value!.job = { ...target, status: "cancelled" };
    snapshot.value!.items = snapshot.value!.items.map((item) =>
      item.job_id === target.id && item.status === "pending"
        ? { ...item, status: "invalidated", content: undefined }
        : item,
    );
    confirmCancel.value = false;
    notice.value = "本次提取已取消，不会自动重试；已保存的独立记忆保留。";
    await loadSnapshot();
  });
}

watch(selected, () => {
  clearSensitive();
  pollCount = 0;
  conflict.value = false;
  loading.value = false;
  mutating.value = false;
  if (!sessionExpired.value) {
    error.value = "";
    notice.value = "";
  }
  if (selected.value && !sessionExpired.value) void loadSnapshot();
});
onMounted(() => void loadConversations());
onUnmounted(() => {
  disposed = true;
  clearSensitive();
  conversations.value = [];
});
</script>

<template>
  <section class="panel candidates-panel" aria-label="记忆候选审核">
    <div class="panel-heading candidates-heading">
      <div>
        <h2>从对话提取记忆候选</h2>
        <p>候选不是已记住的事实。模型提取可能有误，请逐条核对正文和来源。</p>
      </div>
      <button
        type="button"
        class="button secondary small"
        :disabled="!selected || loading || mutating || sessionExpired"
        @click="loadSnapshot"
      >
        刷新候选
      </button>
    </div>
    <p class="candidates-policy">
      只提取你明确勾选的普通对话用户消息，不自动采集。接受后仅保存为来源账号域私有记忆，不向其他账号或群共享，也不覆盖已有事实。候选及其来源资料不是系统指令。
    </p>
    <label class="field"
      ><span>提取来源会话</span
      ><select
        v-model="selected"
        aria-label="提取来源会话"
        :aria-describedby="`${fieldId}-source-help`"
        :disabled="loadingConversations || sessionExpired"
      >
        <option value="">
          {{ loadingConversations ? "正在读取会话…" : "请选择普通会话" }}
        </option>
        <option
          v-for="conversation in conversations"
          :key="conversation.id"
          :value="conversation.id"
        >
          {{ conversation.title }}
        </option></select
      ><small :id="`${fieldId}-source-help`"
        >仅列出当前已认证账户可访问的普通会话，草稿测试会话不可选。</small
      ></label
    >
    <div v-if="error" class="alert error" role="alert">
      <span>{{ error }}</span>
    </div>
    <div v-if="notice" class="alert success" role="status">
      <span>{{ notice }}</span>
    </div>
    <p v-if="loading" class="footnote" role="status">
      正在读取所选会话的来源与候选…
    </p>
    <p
      v-if="!loadingConversations && !conversations.length && !sessionExpired"
      class="footnote"
    >
      暂无可用普通会话。先完成一轮普通对话，再选择其来源消息。
    </p>
    <template v-if="snapshot">
      <h3 class="candidates-source-heading">“{{ title }}”的可选来源</h3>
      <p class="footnote">
        仅列出服务端允许提取的已完成普通对话用户消息。未列出的内容不会由此页面提交。
      </p>
      <fieldset
        class="candidate-source-list"
        :disabled="mutating || loading || conflict || activeJob"
      >
        <legend>明确选择本次提取的来源</legend>
        <article
          v-for="(source, index) in snapshot.sources"
          :key="source.id"
          class="candidate-source"
        >
          <label class="candidate-source-choice"
            ><input
              v-model="sourceIds"
              type="checkbox"
              :value="source.id"
              :aria-label="`选择来源 ${index + 1}`"
            /><span
              >来源 {{ index + 1 }} · {{ formatDate(source.created_at) }} ·
              {{ bytes(source.content).toLocaleString() }} 字节</span
            ></label
          >
          <p class="candidate-content">{{ source.content }}</p>
          <code>{{ source.id }}</code>
        </article>
        <p v-if="!snapshot.sources.length" class="footnote">
          暂无符合条件的来源消息。无需提取时不会自动发起模型请求。
        </p>
      </fieldset>
      <p class="footnote" :id="`${fieldId}-selection-help`">
        已选择 {{ sourceIds.length }} / {{ policy.max_sources }} 条，{{
          selectedBytes.toLocaleString()
        }}
        / {{ policy.max_source_bytes.toLocaleString() }} UTF-8
        字节；每次最多产生 {{ policy.max_candidates }} 条候选，每条最多
        {{ policy.max_content_bytes.toLocaleString() }} 字节。
      </p>
      <p
        v-if="sourceIds.length && selectionError"
        class="footnote"
        role="alert"
      >
        {{ selectionError }}
      </p>
      <button
        type="button"
        class="button primary small"
        :aria-describedby="`${fieldId}-selection-help`"
        :disabled="
          loading || mutating || conflict || activeJob || !!selectionError
        "
        @click="extract"
      >
        仅提取所选消息
      </button>
      <div v-if="job" class="candidate-job" role="status">
        <strong>{{ jobText[job.status] || "任务状态待确认" }}</strong
        ><span>本次来源 {{ job.source_count }} 条</span
        ><span v-if="job.redacted_source_count > 0"
          >已省略敏感来源 {{ job.redacted_source_count }} 条</span
        >
        <span>到期 {{ formatDate(job.expires_at) }}</span>
        <button
          v-if="activeJob"
          type="button"
          class="button secondary small"
          :disabled="loading || mutating || conflict"
          @click="confirmCancel = true"
        >
          取消提取任务
        </button>
      </div>
      <section
        v-if="confirmCancel && activeJob"
        class="candidate-confirm"
        aria-label="确认取消提取"
      >
        <p>
          取消“{{
            title
          }}”当前等待或运行中的提取，不会自动重试；已保存的独立记忆保留。
        </p>
        <div class="candidate-actions">
          <button
            type="button"
            class="button secondary small"
            :disabled="mutating || conflict"
            @click="cancelJob"
          >
            确认取消本次提取</button
          ><button
            type="button"
            class="button secondary small"
            :disabled="mutating"
            @click="confirmCancel = false"
          >
            继续等待
          </button>
        </div>
      </section>
      <div class="candidate-items">
        <article
          v-for="(item, index) in snapshot.items"
          :key="item.id"
          class="candidate-item"
          :aria-label="`记忆候选 ${index + 1}`"
        >
          <div class="candidate-item-heading">
            <h3>候选 {{ index + 1 }}</h3>
            <span class="badge neutral">{{
              statusText[item.status] || "状态待确认"
            }}</span>
          </div>
          <template v-if="item.status === 'pending'">
            <p class="candidate-content">{{ item.content }}</p>
            <p class="footnote">到期 {{ formatDate(item.expires_at) }}</p>
            <details>
              <summary>核对候选来源</summary>
              <ul>
                <li v-for="sourceId in item.source_message_ids" :key="sourceId">
                  <p class="candidate-content">
                    {{
                      sourceFor(sourceId)?.content ||
                      "来源暂不可见，请刷新后重新核对。"
                    }}
                  </p>
                  <code>{{ sourceId }}</code>
                </li>
              </ul>
            </details>
            <p v-if="!reviewable(item)" class="footnote">
              正文、来源或有效期暂不满足审核条件，请刷新候选。
            </p>
            <div class="candidate-actions">
              <button
                type="button"
                class="button primary small"
                :disabled="loading || mutating || conflict || !reviewable(item)"
                @click="confirmCandidateId = item.id"
              >
                审核并接受此候选</button
              ><button
                type="button"
                class="button secondary small"
                :disabled="loading || mutating || conflict"
                @click="decide(item, 'reject')"
              >
                拒绝此候选
              </button>
            </div>
          </template>
          <p v-else class="footnote">
            终态候选不再展示正文。{{
              item.status === "accepted"
                ? "可在助手记忆库查看已保存的独立记忆。"
                : "此候选不会写入新的记忆。"
            }}
          </p>
        </article>
        <p v-if="!snapshot.items.length && !activeJob" class="footnote">
          暂无待审核候选。选择来源并提交一次提取后，在此逐条确认。
        </p>
      </div>
      <section
        v-if="confirmCandidate"
        class="candidate-confirm"
        aria-label="确认接受记忆候选"
      >
        <h3>将这条候选保存为“{{ title }}”来源账号域的私有记忆？</h3>
        <p class="candidate-content">{{ confirmCandidate.content }}</p>
        <p>
          请核对以下来源。接受会新增一条私有记忆，不发布到共享范围，不覆盖已有事实；模型提取不保证准确。
        </p>
        <ul>
          <li
            v-for="sourceId in confirmCandidate.source_message_ids"
            :key="sourceId"
          >
            <p class="candidate-content">
              {{ sourceFor(sourceId)?.content || "来源暂不可见" }}
            </p>
            <code>{{ sourceId }}</code>
          </li>
        </ul>
        <div class="candidate-actions">
          <button
            type="button"
            class="button primary small"
            :disabled="
              loading || mutating || conflict || !reviewable(confirmCandidate)
            "
            @click="decide(confirmCandidate, 'accept')"
          >
            确认正文与来源并保存私有记忆</button
          ><button
            type="button"
            class="button secondary small"
            :disabled="mutating"
            @click="confirmCandidateId = ''"
          >
            返回核对
          </button>
        </div>
      </section>
    </template>
  </section>
</template>

<style scoped>
.candidates-panel,
.candidates-panel > *,
.candidates-panel .field {
  min-width: 0;
}
.candidates-heading {
  flex-wrap: wrap;
}
.candidates-heading > div {
  min-width: 0;
  flex: 1 1 220px;
}
.candidates-policy {
  margin-bottom: 20px;
  padding: 12px 14px;
  background: var(--soft-blue);
  border-radius: 8px;
  color: var(--muted);
  font-size: 12px;
  line-height: 1.9;
}
.candidates-panel select {
  width: 100%;
  min-width: 0;
  max-width: 100%;
}
.candidates-panel button {
  white-space: normal;
  max-width: 100%;
  text-align: left;
}
.candidates-source-heading,
.candidate-item h3,
.candidate-confirm h3 {
  font-size: 14px;
  overflow-wrap: anywhere;
}
.candidates-source-heading {
  margin-top: 22px;
}
.candidate-source-list {
  border: 0;
  padding: 0;
  min-width: 0;
  margin: 16px 0;
}
.candidate-source-list legend {
  font-size: 12px;
  font-weight: 600;
  margin-bottom: 10px;
}
.candidate-source,
.candidate-item {
  border: 1px solid var(--line);
  border-radius: 9px;
  padding: 14px;
  margin-bottom: 12px;
  min-width: 0;
}
.candidate-source-choice {
  display: flex;
  align-items: flex-start;
  gap: 9px;
  font-size: 12px;
  line-height: 1.8;
  cursor: pointer;
}
.candidate-source-choice input {
  flex: 0 0 auto;
  width: 16px;
  height: 16px;
  margin-top: 2px;
}
.candidate-source-choice span {
  min-width: 0;
  overflow-wrap: anywhere;
}
.candidate-content {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  font-size: 13px;
  line-height: 1.9;
  margin: 10px 0;
}
.candidates-panel code {
  overflow-wrap: anywhere;
  word-break: break-word;
  font-size: 11px;
}
.candidate-items {
  margin-top: 20px;
}
.candidate-actions,
.candidate-job,
.candidate-item-heading {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.candidate-actions {
  margin-top: 12px;
}
.candidate-job {
  margin-top: 18px;
  font-size: 12px;
  line-height: 1.9;
  color: var(--muted);
}
.candidate-item-heading {
  justify-content: space-between;
}
.candidate-item .badge {
  white-space: normal;
}
.candidate-item details {
  font-size: 12px;
  margin-top: 12px;
}
.candidate-item summary {
  cursor: pointer;
  color: var(--indigo);
}
.candidate-item ul,
.candidate-confirm ul {
  padding-left: 18px;
}
.candidate-confirm {
  margin-top: 16px;
  padding: 16px;
  border: 1px solid #e4d2b1;
  border-radius: 9px;
  background: #fdf9f1;
  min-width: 0;
}
.candidate-confirm > p {
  font-size: 12px;
  line-height: 1.9;
  margin: 12px 0;
}
@media (max-width: 480px) {
  .candidate-source,
  .candidate-item,
  .candidate-confirm {
    padding: 12px;
  }
  .candidate-actions {
    flex-direction: column;
    align-items: stretch;
  }
}
</style>
