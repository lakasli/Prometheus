<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { api, ApiError, errorMessage } from "../api";
import { formatDate } from "../validation";
import AdminSelect from "./AdminSelect.vue";

export interface Personality {
  name: string;
  description: string;
  personality: string;
  scenario: string;
  system_prompt: string;
  mes_example: string;
}
interface PersonalityState {
  etag: string;
  revision: number;
  personality: Personality;
  saved: boolean;
  applied: boolean;
  effective_from: string;
}
interface PersonalityVersion {
  revision: number;
  personality: Personality;
  created_at: number;
}
const emit = defineEmits<{
  unauthorized: [];
  saved: [personality: Personality];
}>();
const fields = [
  {
    key: "description",
    label: "个人简介",
    limit: 8000,
    rows: 4,
    hint: "她的名字、来历，以及你希望了解的她。",
  },
  {
    key: "personality",
    label: "性情与脾气",
    limit: 4000,
    rows: 4,
    hint: "贯穿所有对话的性格、语气与表达习惯。",
  },
  {
    key: "scenario",
    label: "相处情境",
    limit: 4000,
    rows: 3,
    hint: "彼此如何相处，以及日常交流的情境。",
  },
  {
    key: "system_prompt",
    label: "相处约定",
    limit: 8000,
    rows: 5,
    hint: "表达习惯与相处分寸。修改这些内容不会改变已授予的权限。",
  },
  {
    key: "mes_example",
    label: "对话示例",
    limit: 4000,
    rows: 4,
    hint: "用示例说明期望的说话方式。",
  },
] as const;
const edited = reactive<Personality>({
  name: "",
  description: "",
  personality: "",
  scenario: "",
  system_prompt: "",
  mes_example: "",
});
const state = ref<PersonalityState | null>(null);
const original = ref("");
const loading = ref(false);
const error = ref("");
const notice = ref("");
const conflict = ref(false);
const history = ref<PersonalityVersion[]>([]);
const historyOpen = ref(false);
const historyLoading = ref(false);
const historyRevision = ref("");
const hasUnsaved = computed(
  () => !!state.value && JSON.stringify(edited) !== original.value,
);
let disposed = false;
let controller: AbortController | undefined;
let historyController: AbortController | undefined;
defineExpose({ hasUnsaved });

async function request(save = false) {
  if (loading.value) return;
  if (
    !save &&
    hasUnsaved.value &&
    !window.confirm("重新载入会放弃未保存的性格修改，确定继续？")
  )
    return;
  loading.value = true;
  error.value = "";
  notice.value = "";
  controller = new AbortController();
  try {
    const result = await api<PersonalityState>(
      "/assistant/personality",
      save
        ? {
            method: "PUT",
            etag: state.value!.etag,
            body: { personality: { ...edited } },
            signal: controller.signal,
          }
        : { signal: controller.signal },
    );
    if (disposed) return;
    state.value = result;
    Object.assign(edited, result.personality);
    original.value = JSON.stringify(edited);
    conflict.value = false;
    if (save) {
      notice.value =
        "性格已保存并生效，将用于下一次运行，包括已有会话的后续消息。";
      emit("saved", result.personality);
      if (historyOpen.value) await loadHistory();
    }
  } catch (caught) {
    if (disposed) return;
    if (caught instanceof ApiError && caught.status === 401)
      emit("unauthorized");
    conflict.value = caught instanceof ApiError && caught.status === 409;
    error.value = conflict.value
      ? "性格已被更新。你的修改仍保留，请复制所需内容后重新载入并合并。"
      : errorMessage(caught);
  } finally {
    if (!disposed) loading.value = false;
  }
}
async function loadHistory() {
  if (historyLoading.value || disposed) return;
  historyLoading.value = true;
  historyController = new AbortController();
  try {
    const result = await api<{ items: PersonalityVersion[] }>(
      "/assistant/personality/history",
      { signal: historyController.signal },
    );
    if (disposed) return;
    history.value = result.items;
    if (
      !result.items.some(
        (item) => String(item.revision) === historyRevision.value,
      )
    )
      historyRevision.value = result.items[0]
        ? String(result.items[0].revision)
        : "";
  } catch (caught) {
    if (disposed) return;
    if (caught instanceof ApiError && caught.status === 401)
      emit("unauthorized");
    error.value = errorMessage(caught);
  } finally {
    if (!disposed) historyLoading.value = false;
  }
}
function toggleHistory(event: Event) {
  historyOpen.value = (event.target as HTMLDetailsElement).open;
  if (historyOpen.value) void loadHistory();
}
async function restore() {
  const version = history.value.find(
    (item) => String(item.revision) === historyRevision.value,
  );
  if (!version || loading.value || conflict.value) return;
  if (
    hasUnsaved.value &&
    !window.confirm("恢复历史性格会替换未保存的修改，确定继续？")
  )
    return;
  Object.assign(edited, version.personality);
  await request(true);
}
onMounted(() => void request());
onUnmounted(() => {
  disposed = true;
  controller?.abort();
  historyController?.abort();
});
</script>

<template>
  <section class="panel personality-panel" aria-label="她的性格">
    <div class="panel-heading">
      <div>
        <h2>她的性格</h2>
        <p>在这里写下她的性情、说话方式与相处习惯。</p>
      </div>
      <span v-if="state" class="badge green">已生效 v{{ state.revision }}</span>
    </div>
    <p v-if="error" class="runtime-error" role="alert">{{ error }}</p>
    <p v-if="notice" class="runtime-notice" role="status">{{ notice }}</p>
    <p v-if="!state && loading" class="muted" role="status">
      正在载入性格…
    </p>
    <form v-if="state" @submit.prevent="request(true)">
      <label class="field"
        ><span>姓名</span
        ><input
          v-model="edited.name"
          maxlength="64"
          required
          :disabled="loading"
      /></label>
      <label v-for="field in fields" :key="field.key" class="field">
        <span>{{ field.label }}</span>
        <textarea
          v-model="edited[field.key]"
          :aria-label="field.label"
          :rows="field.rows"
          :maxlength="field.limit"
          :disabled="loading"
        ></textarea>
        <small>{{ field.hint }}</small>
      </label>
      <div class="runtime-actions">
        <button
          type="submit"
          class="button primary"
          :disabled="loading || !hasUnsaved || conflict || !edited.name.trim()"
        >
          保存并应用性格
        </button>
        <button
          type="button"
          class="button secondary"
          :disabled="loading"
          @click="request()"
        >
          重新载入性格
        </button>
        <span class="muted">{{
          hasUnsaved ? "有未保存的性格修改" : "当前性格已生效"
        }}</span>
      </div>
      <p class="footnote">
        保存后供下一次运行使用；进行中的请求和历史运行记录保留当时版本。
      </p>
    </form>
    <details v-if="state" class="personality-history" @toggle="toggleHistory">
      <summary>性格版本与恢复</summary>
      <p class="footnote">
        保留最近 20
        个旧版本。恢复会形成新的性格版本，不回退模型配置或历史运行记录。
      </p>
      <p v-if="historyLoading" class="muted">正在载入性格历史…</p>
      <div v-else-if="history.length" class="runtime-actions">
        <AdminSelect
          v-model="historyRevision"
          aria-label="历史性格版本"
          :options="
            history.map((item) => ({
              value: String(item.revision),
              label: `v${item.revision} · ${formatDate(item.created_at)}`,
            }))
          "
        />
        <button
          class="button secondary small"
          type="button"
          :disabled="loading || historyLoading || conflict"
          @click="restore"
        >
          恢复此版本
        </button>
      </div>
      <p v-else class="muted">尚无可恢复的旧版本。</p>
    </details>
    <button
      v-if="!state && !loading"
      class="button secondary"
      @click="request()"
    >
      重试载入性格
    </button>
  </section>
</template>
