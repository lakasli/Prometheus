<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, useId } from "vue";
import { api, ApiError, errorMessage } from "../api";
import { formatDate } from "../validation";
import type { Conversation, Timestamp } from "../types";
import AdminSelect from "./AdminSelect.vue";

type Visibility = "shared" | "private";
interface MemoryItem {
  id: string;
  assistant_id: string;
  content: string;
  visibility: Visibility;
  source_conversation_id: string | null;
  source_message_id: string | null;
  revision: number;
  created_at: Timestamp;
  updated_at: Timestamp;
}
interface MemoryList {
  assistant_id?: string;
  items: MemoryItem[];
  total: number;
  limit: number;
  offset: number;
}

const limit = 25;
const byteLimit = 4000;
const fieldId = useId();
const assistantId = ref("");
const conversations = ref<Conversation[]>([]);
const items = ref<MemoryItem[]>([]);
const total = ref(0);
const offset = ref(0);
const loading = ref(false);
const busy = ref("");
const error = ref("");
const notice = ref("");
const filters = reactive({ q: "", visibility: "", source: "" });
const activeFilters = ref({ q: "", visibility: "", source: "" });
const createForm = reactive({
  content: "",
  visibility: "private" as Visibility,
  source: "",
  sourceMessage: "",
  publish: false,
});
const editing = ref<MemoryItem | null>(null);
const editForm = reactive({
  content: "",
  visibility: "private" as Visibility,
  publish: false,
});
const conflict = ref(false);
const latest = ref<MemoryItem | null>(null);
const forgetTarget = ref<MemoryItem | null>(null);
let controller: AbortController | null = null;
let requestEpoch = 0;
let disposed = false;

const createBytes = computed(() => bytes(createForm.content));
const editBytes = computed(() => bytes(editForm.content));
const editNeedsPublish = computed(
  () =>
    editing.value?.visibility === "private" && editForm.visibility === "shared",
);
const pageLabel = computed(() =>
  total.value
    ? `${offset.value + 1}–${Math.min(offset.value + items.value.length, total.value)} / ${total.value}`
    : "0 条记忆",
);

function bytes(value: string) {
  return new TextEncoder().encode(value).length;
}
function contentError(value: string) {
  if (!value.trim()) return "请填写要保存的记忆内容。";
  if (bytes(value) > byteLimit) return "记忆内容不能超过 4,000 UTF-8 字节。";
  return "";
}
function itemPath(id: string) {
  return `/memory/${encodeURIComponent(id)}`;
}
function sourceTitle(id: string | null) {
  if (!id) return "管理员直接录入";
  const conversation = conversations.value.find((item) => item.id === id);
  return conversation ? `${conversation.title}（${id}）` : id;
}
function visibilityTitle(value: Visibility) {
  return value === "shared" ? "跨渠道共享" : "来源账号域私有";
}
async function loadList() {
  controller?.abort();
  const request = new AbortController();
  controller = request;
  const epoch = ++requestEpoch;
  loading.value = true;
  const params = new URLSearchParams({
    limit: String(limit),
    offset: String(offset.value),
  });
  if (activeFilters.value.q) params.set("q", activeFilters.value.q);
  if (activeFilters.value.visibility)
    params.set("visibility", activeFilters.value.visibility);
  if (activeFilters.value.source)
    params.set("source_conversation_id", activeFilters.value.source);
  try {
    const result = await api<MemoryList>(`/memory?${params}`, {
      signal: request.signal,
    });
    if (disposed || request.signal.aborted || epoch !== requestEpoch) return;
    items.value = result.items;
    total.value = result.total;
    assistantId.value =
      result.assistant_id || result.items[0]?.assistant_id || assistantId.value;
  } catch (caught) {
    if (!disposed && !request.signal.aborted && epoch === requestEpoch)
      error.value = `记忆列表未能更新。${errorMessage(caught)}`;
  } finally {
    if (epoch === requestEpoch) loading.value = false;
  }
}
async function task(label: string, action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = label;
  error.value = "";
  notice.value = "";
  try {
    await action();
  } catch (caught) {
    error.value = errorMessage(caught);
  } finally {
    busy.value = "";
  }
}
async function initialize() {
  const results = await Promise.allSettled([
    loadList(),
    api<{ items: Conversation[] }>("/conversations"),
  ]);
  if (disposed) return;
  const conversationResult = results[1];
  if (conversationResult?.status === "fulfilled")
    conversations.value = conversationResult.value.items.filter(
      (conversation) =>
        !conversation.test &&
        !conversation.is_test &&
        !conversation.character?.is_test,
    );
  else if (conversationResult?.status === "rejected")
    error.value = `来源会话未能载入。${errorMessage(conversationResult.reason)}`;
}
function search() {
  activeFilters.value = {
    q: filters.q.trim(),
    visibility: filters.visibility,
    source: filters.source,
  };
  offset.value = 0;
  error.value = "";
  void loadList();
}
function changePage(next: number) {
  offset.value = Math.max(0, next);
  error.value = "";
  void loadList();
}
async function createMemory() {
  error.value = contentError(createForm.content);
  if (error.value) return;
  if (createForm.visibility === "private" && !createForm.source) {
    error.value = "私有记忆必须选择来源会话，以确定保密的账号域。";
    return;
  }
  if (createForm.visibility === "shared" && !createForm.publish) {
    error.value = "请明确确认允许在所有渠道使用这条共享记忆。";
    return;
  }
  await task("保存记忆", async () => {
    const saved = await api<MemoryItem>("/memory", {
      method: "POST",
      body: {
        content: createForm.content.trim(),
        visibility: createForm.visibility,
        ...(createForm.source
          ? { source_conversation_id: createForm.source }
          : {}),
        ...(createForm.sourceMessage.trim()
          ? { source_message_id: createForm.sourceMessage.trim() }
          : {}),
        ...(createForm.visibility === "shared" ? { publish: true } : {}),
      },
    });
    assistantId.value = saved.assistant_id;
    createForm.content = "";
    createForm.sourceMessage = "";
    createForm.publish = false;
    notice.value = `记忆已保存，版本 ${saved.revision}，范围：${visibilityTitle(saved.visibility)}。`;
    offset.value = 0;
    await loadList();
  });
}
function beginEdit(item: MemoryItem) {
  editing.value = { ...item };
  editForm.content = item.content;
  editForm.visibility = item.visibility;
  editForm.publish = false;
  conflict.value = false;
  latest.value = null;
  forgetTarget.value = null;
  error.value = "";
  notice.value = "";
}
function cancelEdit() {
  editing.value = null;
  editForm.content = "";
  editForm.publish = false;
  conflict.value = false;
  latest.value = null;
}
async function saveEdit() {
  const item = editing.value;
  if (!item || conflict.value) return;
  error.value = contentError(editForm.content);
  if (error.value) return;
  if (editNeedsPublish.value && !editForm.publish) {
    error.value = "将私有记忆改为共享前，请明确确认跨渠道使用。";
    return;
  }
  await task("保存纠正", async () => {
    try {
      const saved = await api<MemoryItem>(itemPath(item.id), {
        method: "PATCH",
        revision: item.revision,
        body: {
          content: editForm.content.trim(),
          ...(editForm.visibility !== item.visibility
            ? { visibility: editForm.visibility }
            : {}),
          ...(editNeedsPublish.value ? { publish: true } : {}),
        },
      });
      cancelEdit();
      notice.value = `纠正已保存，版本 ${saved.revision}。`;
      await loadList();
    } catch (caught) {
      if (caught instanceof ApiError && caught.status === 409) {
        conflict.value = true;
        error.value =
          "这条记忆已在别处更新。你的编辑草稿仍保留，请载入最新版本并对照合并。";
        return;
      }
      throw caught;
    }
  });
}
async function loadLatest() {
  if (!editing.value) return;
  await task("载入最新版本", async () => {
    latest.value = await api<MemoryItem>(itemPath(editing.value!.id));
    notice.value = "最新版本已载入。编辑草稿保持原样，请对照后确认合并。";
  });
}
function adoptLatest() {
  if (!latest.value) return;
  editing.value = { ...latest.value };
  latest.value = null;
  conflict.value = false;
  editForm.publish = false;
  error.value = "";
  notice.value =
    "已采用最新版本号，草稿内容保持原样；检查内容与可见范围后再保存。";
}
function askForget(item: MemoryItem) {
  forgetTarget.value = { ...item };
  error.value = "";
  notice.value = "";
}
async function forgetMemory() {
  const item = forgetTarget.value;
  if (!item) return;
  await task("遗忘记忆", async () => {
    try {
      await api(itemPath(item.id), {
        method: "DELETE",
        revision: item.revision,
      });
      items.value = items.value.filter((entry) => entry.id !== item.id);
      total.value = Math.max(0, total.value - 1);
      forgetTarget.value = null;
      if (editing.value?.id === item.id) cancelEdit();
      notice.value =
        "该记忆库条目及其旧版本已清除。原始消息与离线备份仍独立保留。";
      if (!items.value.length && offset.value)
        offset.value = Math.max(0, offset.value - limit);
      await loadList();
    } catch (caught) {
      if (caught instanceof ApiError && caught.status === 409) {
        forgetTarget.value = null;
        error.value =
          "记忆已更新，尚未遗忘。请刷新列表，核对最新内容后重新确认。";
        return;
      }
      throw caught;
    }
  });
}
async function exportMemory() {
  await task("导出记忆", async () => {
    const data = await api<unknown>("/memory/export");
    const url = URL.createObjectURL(
      new Blob([JSON.stringify(data, null, 2)], {
        type: "application/json;charset=utf-8",
      }),
    );
    const link = document.createElement("a");
    link.href = url;
    link.download = "dsh-memories.json";
    document.body.appendChild(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 0);
    notice.value = "已导出记忆库的有效条目。文件包含私有内容，请妥善保存。";
  });
}

defineExpose({ refresh: loadList });
onMounted(() => void initialize());
onUnmounted(() => {
  disposed = true;
  controller?.abort();
});
</script>

<template>
  <div class="memory-workspace">
    <section class="panel">
      <div class="panel-heading memory-heading">
        <div>
          <h2>记忆库</h2>
          <p>
            记忆不会因渠道、会话或性格版本变化而丢失。共享条目可用于各渠道，私有条目仅用于来源账号域。
          </p>
          <p v-if="assistantId" class="memory-identity">
            记忆归属标识
            <code :title="assistantId">{{ assistantId.slice(0, 12) }}…</code>
          </p>
        </div>
        <button
          type="button"
          class="button secondary small"
          :disabled="!!busy"
          @click="exportMemory"
        >
          导出记忆
        </button>
      </div>
      <p class="memory-scope-note">
        这里只保存你明确录入或审核通过的内容。私有记忆是为来源账号保留的保密记录；公开共享需要管理员明确确认。请勿录入密码、验证码或密钥。
      </p>
      <div v-if="error" role="alert" class="alert error">
        <span>{{ error }}</span>
      </div>
      <div v-if="notice" role="status" class="alert success">
        <span>{{ notice }}</span>
      </div>
      <form
        class="memory-filters"
        aria-label="筛选记忆"
        @submit.prevent="search"
      >
        <label class="field"
          ><span>查询内容</span
          ><input
            v-model="filters.q"
            aria-label="查询内容"
            type="search"
            placeholder="输入关键词"
            :disabled="!!busy"
        /></label>
        <label class="field"
          ><span>可见范围</span
          ><AdminSelect
            v-model="filters.visibility"
            aria-label="可见范围"
            :disabled="!!busy"
            :options="[
              { value: '', label: '全部范围' },
              { value: 'shared', label: '跨渠道共享' },
              { value: 'private', label: '来源账号域私有' },
            ]"
        /></label>
        <label class="field"
          ><span>来源会话</span
          ><AdminSelect
            v-model="filters.source"
            aria-label="来源会话"
            :disabled="!!busy"
            :options="[
              { value: '', label: '全部来源' },
              ...conversations.map((conversation) => ({
                value: conversation.id,
                label: conversation.title,
              })),
            ]"
        /></label>
        <button
          type="submit"
          class="button secondary"
          :disabled="!!busy || loading"
        >
          查询
        </button>
      </form>
      <div class="memory-list-heading">
        <span class="muted">{{ loading ? "正在读取记忆…" : pageLabel }}</span
        ><button
          type="button"
          class="link-button"
          :disabled="!!busy || loading"
          @click="loadList"
        >
          刷新列表
        </button>
      </div>
      <div v-if="!loading && !items.length" class="empty-state">
        <h3>暂无符合条件的记忆</h3>
        <p>可在下方明确保存一条内容，或调整查询条件。</p>
      </div>
      <div class="memory-list" :aria-busy="loading">
        <article
          v-for="item in items"
          :key="item.id"
          class="memory-item"
          :data-memory-id="item.id"
        >
          <div class="memory-item-top">
            <span
              class="badge"
              :class="item.visibility === 'shared' ? 'green' : 'neutral'"
              >{{ visibilityTitle(item.visibility) }}</span
            ><span class="muted">版本 {{ item.revision }}</span>
          </div>
          <p class="memory-content">{{ item.content }}</p>
          <dl class="memory-metadata">
            <div>
              <dt>来源</dt>
              <dd>{{ sourceTitle(item.source_conversation_id) }}</dd>
            </div>
            <div v-if="item.source_message_id">
              <dt>源消息</dt>
              <dd>{{ item.source_message_id }}</dd>
            </div>
            <div>
              <dt>建立</dt>
              <dd>{{ formatDate(item.created_at) }}</dd>
            </div>
            <div>
              <dt>更新</dt>
              <dd>{{ formatDate(item.updated_at) }}</dd>
            </div>
          </dl>
          <div class="memory-actions">
            <button
              type="button"
              class="button secondary small"
              :disabled="!!busy || !!editing"
              @click="beginEdit(item)"
            >
              编辑纠正</button
            ><button
              type="button"
              class="button secondary small"
              :disabled="!!busy"
              @click="askForget(item)"
            >
              遗忘此条
            </button>
          </div>
        </article>
      </div>
      <nav class="memory-pagination" aria-label="记忆分页">
        <button
          type="button"
          class="button secondary small"
          :disabled="!!busy || loading || offset === 0"
          @click="changePage(offset - limit)"
        >
          上一页</button
        ><span class="muted">{{ pageLabel }}</span
        ><button
          type="button"
          class="button secondary small"
          :disabled="!!busy || loading || offset + limit >= total"
          @click="changePage(offset + limit)"
        >
          下一页
        </button>
      </nav>
      <section
        v-if="forgetTarget"
        class="memory-confirm"
        role="region"
        aria-label="确认遗忘"
      >
        <h3>确认遗忘这条记忆？</h3>
        <p class="memory-content">{{ forgetTarget.content }}</p>
        <p class="footnote">
          {{ visibilityTitle(forgetTarget.visibility) }} · 版本
          {{
            forgetTarget.revision
          }}。此操作清除记忆库中的文本与旧版本；原始消息与离线备份仍独立保留。
        </p>
        <div class="memory-actions">
          <button
            type="button"
            class="button primary small"
            :disabled="!!busy"
            @click="forgetMemory"
          >
            确认遗忘此条</button
          ><button
            type="button"
            class="button secondary small"
            :disabled="!!busy"
            @click="forgetTarget = null"
          >
            取消遗忘
          </button>
        </div>
      </section>
    </section>

    <section v-if="editing" class="panel" aria-label="编辑记忆">
      <div class="panel-heading">
        <div>
          <h2>纠正记忆 · 版本 {{ editing.revision }}</h2>
          <p>来源记录保持不变。修改内容后会建立新版本。</p>
        </div>
      </div>
      <form @submit.prevent="saveEdit">
        <label class="field"
          ><span>纠正后的内容</span
          ><textarea
            v-model="editForm.content"
            aria-label="纠正后的内容"
            :aria-describedby="`${fieldId}-edit-bytes`"
            rows="5"
            :disabled="!!busy"
          /><small
            :id="`${fieldId}-edit-bytes`"
            :class="{ 'memory-invalid': editBytes > byteLimit }"
            >{{ editBytes.toLocaleString() }} / 4,000 UTF-8 字节</small
          ></label
        >
        <label class="field"
          ><span>纠正后的可见范围</span
          ><AdminSelect
            v-model="editForm.visibility"
            aria-label="纠正后的可见范围"
            :aria-describedby="
              !editing.source_conversation_id
                ? `${fieldId}-edit-source`
                : undefined
            "
            :disabled="!!busy"
            :options="[
              { value: 'shared', label: '跨渠道共享' },
              {
                value: 'private',
                label: '来源账号域私有',
                disabled: !editing.source_conversation_id,
              },
            ]"
          /><small
            v-if="!editing.source_conversation_id"
            :id="`${fieldId}-edit-source`"
            >此条没有来源会话。如需改为私有，请选定来源会话新建私有条目，再遗忘这条共享记忆。</small
          ></label
        >
        <label v-if="editNeedsPublish" class="memory-checkbox"
          ><input
            v-model="editForm.publish"
            aria-label="允许在所有渠道使用这条记忆，包括群聊。"
            type="checkbox"
            :disabled="!!busy"
          /><span>允许在所有渠道使用这条记忆，包括群聊。</span></label
        >
        <section v-if="conflict" class="memory-conflict">
          <p>草稿仍保留。先载入最新版本，再对照内容合并，避免覆盖其他修改。</p>
          <button
            type="button"
            class="button secondary small"
            :disabled="!!busy"
            @click="loadLatest"
          >
            载入最新版本</button
          ><template v-if="latest"
            ><p>
              最新内容 · 版本 {{ latest.revision }} ·
              {{ visibilityTitle(latest.visibility) }}
            </p>
            <p class="memory-content">{{ latest.content }}</p>
            <button
              type="button"
              class="button secondary small"
              :disabled="!!busy"
              @click="adoptLatest"
            >
              已对照，采用最新版本号并保留草稿
            </button></template
          >
        </section>
        <div class="memory-actions">
          <button
            type="submit"
            class="button primary"
            :disabled="
              !!busy ||
              conflict ||
              !editForm.content.trim() ||
              editBytes > byteLimit
            "
          >
            保存纠正</button
          ><button
            type="button"
            class="button secondary"
            :disabled="!!busy"
            @click="cancelEdit"
          >
            取消编辑
          </button>
        </div>
      </form>
    </section>

    <section class="panel" aria-label="新增记忆">
      <div class="panel-heading">
        <div>
          <h2>明确保存一条记忆</h2>
          <p>写下希望保留的事实或经历，并选择使用范围。</p>
        </div>
      </div>
      <form @submit.prevent="createMemory">
        <label class="field"
          ><span>记忆内容</span
          ><textarea
            v-model="createForm.content"
            aria-label="记忆内容"
            :aria-describedby="`${fieldId}-create-bytes`"
            rows="5"
            placeholder="例如：已经确认的偏好、共同经历或项目事实"
            :disabled="!!busy"
          /><small
            :id="`${fieldId}-create-bytes`"
            :class="{ 'memory-invalid': createBytes > byteLimit }"
            >{{ createBytes.toLocaleString() }} / 4,000 UTF-8 字节</small
          ></label
        >
        <div class="field-row">
          <label class="field"
            ><span>使用范围</span
            ><AdminSelect
              v-model="createForm.visibility"
              aria-label="使用范围"
              :disabled="!!busy"
              @change="createForm.publish = false"
              :options="[
                { value: 'private', label: '来源账号域私有' },
                { value: 'shared', label: '跨渠道共享' },
              ]" /></label
          ><label class="field"
            ><span
              >来源会话
              {{
                createForm.visibility === "private" ? "（必选）" : "（可选）"
              }}</span
            ><AdminSelect
              v-model="createForm.source"
              aria-label="来源会话"
              :aria-required="createForm.visibility === 'private'"
              :aria-describedby="`${fieldId}-create-source`"
              :disabled="!!busy"
              @change="createForm.sourceMessage = ''"
              :options="[
                { value: '', label: '请选择来源会话' },
                ...conversations.map((conversation) => ({
                  value: conversation.id,
                  label: conversation.title,
                })),
              ]"
            /><small :id="`${fieldId}-create-source`"
              >私有条目仅可在该来源账号域内使用，不自动进入其他账号或群聊。</small
            ></label
          >
        </div>
        <label class="field"
          ><span>来源消息 ID（可选）</span
          ><input
            v-model="createForm.sourceMessage"
            aria-label="来源消息 ID（可选）"
            :aria-describedby="`${fieldId}-create-message`"
            :disabled="!!busy || !createForm.source"
            placeholder="来源会话中的消息 ID"
          /><small :id="`${fieldId}-create-message`"
            >用于追溯；来源会话与消息的归属由服务端核验。</small
          ></label
        >
        <label v-if="createForm.visibility === 'shared'" class="memory-checkbox"
          ><input
            v-model="createForm.publish"
            aria-label="允许在所有渠道使用这条记忆，包括群聊。"
            type="checkbox"
            :disabled="!!busy"
          /><span>允许在所有渠道使用这条记忆，包括群聊。</span></label
        >
        <button
          type="submit"
          class="button primary"
          :disabled="
            !!busy || !createForm.content.trim() || createBytes > byteLimit
          "
        >
          保存记忆
        </button>
      </form>
    </section>
  </div>
</template>

<style scoped>
.memory-workspace {
  display: grid;
  gap: 20px;
  min-width: 0;
}
.memory-workspace > *,
.memory-workspace form,
.memory-workspace .field {
  min-width: 0;
}
.memory-heading {
  flex-wrap: wrap;
}
.memory-heading > div {
  flex: 1 1 260px;
  min-width: 0;
}
.memory-identity code {
  color: var(--indigo);
}
.memory-scope-note {
  font-size: 12px;
  line-height: 1.9;
  color: var(--muted);
  background: var(--soft-blue);
  border-radius: 8px;
  padding: 12px 14px;
  margin: 0 0 20px;
}
.memory-filters {
  display: grid;
  grid-template-columns:
    minmax(130px, 1.2fr) minmax(120px, 1fr) minmax(120px, 1fr)
    auto;
  gap: 12px;
  align-items: end;
  margin-bottom: 18px;
}
.memory-filters .field {
  margin: 0;
}
.memory-list-heading,
.memory-item-top,
.memory-actions,
.memory-pagination {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.memory-list-heading {
  justify-content: space-between;
  font-size: 12px;
  margin: 0 0 12px;
}
.memory-list {
  display: grid;
  gap: 14px;
}
.memory-item {
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 16px;
  min-width: 0;
}
.memory-item-top {
  font-size: 11px;
  justify-content: space-between;
}
.memory-content {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  font-size: 13px;
  line-height: 1.9;
  margin: 13px 0;
}
.memory-metadata {
  font-size: 11px;
  color: var(--muted);
  margin: 0 0 12px;
}
.memory-metadata > div {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin: 4px 0;
}
.memory-metadata dt {
  flex: 0 0 38px;
}
.memory-metadata dd {
  margin: 0;
  min-width: 0;
  overflow-wrap: anywhere;
}
.memory-pagination {
  justify-content: space-between;
  font-size: 12px;
  padding-top: 20px;
}
.memory-confirm,
.memory-conflict {
  padding: 16px;
  border: 1px solid #e4d2b1;
  border-radius: 9px;
  background: #fdf9f1;
  margin-top: 18px;
}
.memory-confirm h3 {
  font-size: 14px;
}
.memory-confirm .memory-actions {
  margin-top: 14px;
}
.memory-conflict {
  margin: 0 0 18px;
  font-size: 12px;
  line-height: 1.9;
}
.memory-conflict > p {
  margin-bottom: 12px;
}
.memory-conflict > button {
  white-space: normal;
  text-align: left;
}
.memory-checkbox {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  font-size: 12px;
  line-height: 1.8;
  margin: 0 0 18px;
}
.memory-checkbox input {
  width: 16px;
  height: 16px;
  flex: 0 0 16px;
  margin-top: 3px;
}
.memory-invalid {
  color: #a64e42 !important;
}
.memory-workspace input,
.memory-workspace select,
.memory-workspace textarea {
  width: 100%;
  min-width: 0;
  max-width: 100%;
}
.memory-workspace textarea {
  resize: vertical;
}
@media (max-width: 950px) {
  .memory-filters {
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  }
}
@media (max-width: 480px) {
  .memory-filters {
    grid-template-columns: minmax(0, 1fr);
  }
  .memory-pagination {
    justify-content: center;
  }
  .memory-pagination > span {
    flex-basis: 100%;
    text-align: center;
    order: -1;
  }
  .memory-item {
    padding: 13px;
  }
}
</style>
