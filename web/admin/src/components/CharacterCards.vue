<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { api, ApiError, errorMessage } from "../api";
import { formatDate } from "../validation";
import AdminSelect from "./AdminSelect.vue";
import type {
  CharacterCard,
  CharacterCardList,
  CharacterCardSummary,
  CharacterDraft,
  CharacterRevision,
  Conversation,
  FieldError,
} from "../types";

const emit = defineEmits<{
  changed: [];
  testConversation: [conversation: Conversation];
  authExpired: [];
}>();
const library = ref<CharacterCardList>({
  items: [],
  default: null,
  default_etag: "0",
});
const draft = ref<CharacterDraft | null>(null);
const edited = ref<CharacterCard | null>(null);
const savedJson = ref("");
const working = ref("");
const error = ref("");
const notice = ref("");
const revisions = ref<CharacterRevision[]>([]);
const revisionChoice = ref("");
const importInput = ref<HTMLInputElement>();
const validation = ref<{
  valid: boolean;
  bindable?: boolean;
  errors: FieldError[];
  warnings?: FieldError[];
} | null>(null);
const preview = ref<{
  system_prompt: string;
  bytes: number;
  warnings?: FieldError[];
  ignored_fields?: string[];
} | null>(null);
const imported = ref<{
  import_id: string;
  digest: string;
  card: CharacterCard;
  warnings: FieldError[];
  dropped_fields: string[];
  issues?: FieldError[];
} | null>(null);
let publishAttempt: { draftId: string; etag: string; key: string } | null =
  null;
const hasUnsaved = computed(
  () => !!edited.value && JSON.stringify(edited.value) !== savedJson.value,
);
const selectedSummary = computed(() =>
  library.value.items.find((item) => item.id === draft.value?.card_id),
);
const selectedPublished = computed(() =>
  revisions.value.find(
    (item) => item.revision === Number(revisionChoice.value),
  ),
);
const data = computed(() => edited.value?.data);
const tags = computed({
  get: () => data.value?.tags.join("，") || "",
  set: (value: string) => {
    if (data.value)
      data.value.tags = value
        .split(/[,，]/)
        .map((tag) => tag.trim())
        .filter(Boolean);
  },
});
const greetings = computed({
  get: () => data.value?.alternate_greetings.join("\n") || "",
  set: (value: string) => {
    if (data.value)
      data.value.alternate_greetings = value.split("\n").filter(Boolean);
  },
});
const groupGreetings = computed({
  get: () => data.value?.group_only_greetings.join("\n") || "",
  set: (value: string) => {
    if (data.value)
      data.value.group_only_greetings = value.split("\n").filter(Boolean);
  },
});
const modulesNeedReset = computed(
  () =>
    !!data.value &&
    (data.value.extensions.dsh.modules.chat.mode !== "inherit" ||
      data.value.extensions.dsh.modules.vision.mode !== "disabled" ||
      data.value.extensions.dsh.modules.speech.mode !== "disabled"),
);
watch(
  edited,
  () => {
    validation.value = null;
    preview.value = null;
  },
  { deep: true },
);
defineExpose({ hasUnsaved });

async function perform(label: string, action: () => Promise<unknown>) {
  if (working.value) return;
  working.value = label;
  error.value = "";
  notice.value = "";
  try {
    return await action();
  } catch (caught) {
    error.value = errorMessage(caught);
    if (caught instanceof ApiError && caught.status === 401)
      emit("authExpired");
  } finally {
    working.value = "";
  }
}
async function refreshLibrary() {
  library.value = await api<CharacterCardList>("/character-cards");
}
function mayLeave() {
  return (
    !hasUnsaved.value ||
    window.confirm("角色卡有未保存的修改，确定放弃这些修改？")
  );
}
function setDraft(value: CharacterDraft) {
  draft.value = value;
  edited.value = JSON.parse(JSON.stringify(value.card));
  savedJson.value = JSON.stringify(edited.value);
  validation.value = null;
  preview.value = null;
}
function draftPath() {
  if (!draft.value) throw new Error("No draft");
  return `/character-cards/${encodeURIComponent(draft.value.card_id)}/drafts/${encodeURIComponent(draft.value.draft_id)}`;
}
async function refreshRevisions() {
  if (!draft.value) return;
  revisions.value = (
    await api<{ items: CharacterRevision[] }>(
      `/character-cards/${encodeURIComponent(draft.value.card_id)}/revisions`,
    )
  ).items;
  revisionChoice.value = String(revisions.value[0]?.revision || "");
}
async function createCard() {
  if (!mayLeave()) return;
  await perform("创建卡片", async () => {
    setDraft(
      await api<CharacterDraft>("/character-cards", {
        method: "POST",
        body: {},
      }),
    );
    await refreshLibrary();
    await refreshRevisions();
    notice.value = "已创建独立草稿。保存、发布和设为默认是三个独立动作。";
  });
}
async function openCard(item: CharacterCardSummary) {
  if (!mayLeave()) return;
  await perform("读取卡片", async () => {
    if (item.archived_at && !item.draft?.card && item.latest_revision) {
      const card = await api<CharacterCard>(
        `/character-cards/${encodeURIComponent(item.id)}/revisions/${item.latest_revision}/export`,
      );
      setDraft({
        card_id: item.id,
        draft_id: "archived",
        draft_etag: "",
        card,
      });
      await refreshRevisions();
      return;
    }
    const next = item.draft?.card
      ? { ...item.draft, card_id: item.id }
      : await api<CharacterDraft>(
          `/character-cards/${encodeURIComponent(item.id)}/drafts`,
          { method: "POST", body: { revision: item.latest_revision } },
        );
    setDraft(next);
    await refreshRevisions();
  });
}
async function saveCurrent() {
  if (!draft.value || !edited.value) return;
  if (!hasUnsaved.value) return;
  setDraft(
    await api<CharacterDraft>(draftPath(), {
      method: "PUT",
      body: { card: edited.value },
      etag: draft.value.draft_etag,
    }),
  );
  await refreshLibrary();
}
async function save() {
  await perform("保存草稿", async () => {
    await saveCurrent();
    notice.value = "草稿已保存，已发布版本和运行中的会话保持原版本。";
  });
}
async function validate() {
  await perform("校验卡片", async () => {
    await saveCurrent();
    validation.value = await api(draftPath() + "/validate", { method: "POST" });
  });
}
async function compilePreview() {
  await perform("编译预览", async () => {
    await saveCurrent();
    preview.value = await api(draftPath() + "/preview", { method: "POST" });
  });
}
async function publish() {
  if (!draft.value || !validation.value?.valid || hasUnsaved.value) return;
  await perform("发布版本", async () => {
    if (
      !publishAttempt ||
      publishAttempt.draftId !== draft.value!.draft_id ||
      publishAttempt.etag !== draft.value!.draft_etag
    )
      publishAttempt = {
        draftId: draft.value!.draft_id,
        etag: draft.value!.draft_etag,
        key: crypto.randomUUID(),
      };
    const published = await api<CharacterRevision & { draft_etag?: string }>(
      draftPath() + "/publish",
      {
        method: "POST",
        etag: draft.value!.draft_etag,
        idempotencyKey: publishAttempt.key,
      },
    );
    if (published.draft_etag) draft.value!.draft_etag = published.draft_etag;
    validation.value = null;
    await refreshLibrary();
    await refreshRevisions();
    revisionChoice.value = String(published.revision);
    notice.value = `已发布 v${published.revision}，现有会话和新会话默认值尚未变更。`;
    emit("changed");
  });
}
async function setDefault() {
  const value = selectedPublished.value;
  if (!value) return;
  await perform("设置默认", async () => {
    await api("/character-cards/default", {
      method: "PUT",
      body: { card_id: value.card_id, revision: value.revision },
      etag: library.value.default_etag,
    });
    await refreshLibrary();
    notice.value = `新会话默认已设为 ${value.name} v${value.revision}；已有会话使用各自绑定版本。`;
    emit("changed");
  });
}
async function editPublishedVersion() {
  const value = selectedPublished.value;
  if (!value || !mayLeave()) return;
  await perform("从版本创建草稿", async () => {
    setDraft(
      await api<CharacterDraft>(
        `/character-cards/${encodeURIComponent(value.card_id)}/drafts`,
        { method: "POST", body: { revision: value.revision } },
      ),
    );
    await refreshLibrary();
    notice.value = `已从 v${value.revision} 创建独立草稿。发布后会生成新版本，不会改写历史。`;
  });
}
async function archive() {
  const item = selectedSummary.value;
  if (
    !item ||
    !window.confirm("归档后不能创建新绑定，已有会话仍保留原版本。确定归档？")
  )
    return;
  await perform("归档卡片", async () => {
    await api(`/character-cards/${encodeURIComponent(item.id)}/archive`, {
      method: "POST",
      etag: String(item.latest_revision || 0),
    });
    await refreshLibrary();
    notice.value = "卡片已归档，历史内容已保留。";
    emit("changed");
  });
}
async function setQqAccess(event: Event) {
  const item = selectedSummary.value;
  if (!item?.latest_revision) return;
  const enabled = (event.target as HTMLInputElement).checked;
  await perform("更新QQ可选范围", async () => {
    await api(`/character-cards/${encodeURIComponent(item.id)}/qq-access`, {
      method: "PUT",
      body: { enabled },
      etag: String(item.latest_revision),
    });
    await refreshLibrary();
    notice.value = enabled
      ? "此角色已允许QQ私聊用户选择。发布新版本仍不会改变已有会话。"
      : "此角色已从QQ可选列表移除。已有绑定及实例默认不变。";
  });
}
async function exportCard() {
  const value = selectedPublished.value;
  if (!value) return;
  await perform("导出卡片", async () => {
    const card = await api<CharacterCard>(
      `/character-cards/${encodeURIComponent(value.card_id)}/revisions/${value.revision}/export`,
    );
    const url = URL.createObjectURL(
      new Blob([JSON.stringify(card, null, 2)], { type: "application/json" }),
    );
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `dsh-character-v${value.revision}.json`;
    anchor.click();
    URL.revokeObjectURL(url);
    notice.value = "已导出人格 JSON。导出内容不包含会话和本机模块绑定。";
  });
}
async function importFile(event: Event) {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (!file) return;
  if (file.size > 8 * 1024 * 1024) {
    error.value = "文件超过8 MiB限制；大型形象包请先导出文本JSON。";
    return;
  }
  await perform("解析导入", async () => {
    imported.value = await api("/character-cards/import/inspect", {
      method: "POST",
      file,
      contentType: file.name.toLowerCase().endsWith(".zip")
        ? "application/zip"
        : "application/json",
    });
  });
}
async function confirmImport() {
  if (!imported.value || !mayLeave()) return;
  await perform("确认导入", async () => {
    setDraft(
      await api<CharacterDraft>(
        `/character-cards/import/${encodeURIComponent(imported.value!.import_id)}/confirm`,
        {
          method: "POST",
          body: { card: imported.value!.card, digest: imported.value!.digest },
        },
      ),
    );
    imported.value = null;
    await refreshLibrary();
    await refreshRevisions();
    notice.value = "转换内容已保存为新草稿，尚未发布或激活。";
  });
}
function resetModules(card = edited.value) {
  if (!card) return;
  card.data.extensions.dsh.modules = {
    chat: { mode: "inherit" },
    vision: { mode: "disabled" },
    speech: { mode: "disabled" },
  };
}
async function testConversation() {
  if (!draft.value) return;
  await perform("创建测试会话", async () => {
    await saveCurrent();
    const conversation = await api<Conversation>(
      draftPath() + "/test-conversations",
      { method: "POST", etag: draft.value!.draft_etag },
    );
    emit("testConversation", conversation);
  });
}
function beforeUnload(event: BeforeUnloadEvent) {
  if (hasUnsaved.value) {
    event.preventDefault();
    event.returnValue = "";
  }
}
onMounted(() => {
  window.addEventListener("beforeunload", beforeUnload);
  void perform("读取卡库", refreshLibrary);
});
onBeforeUnmount(() => window.removeEventListener("beforeunload", beforeUnload));
</script>

<template>
  <div class="character-workspace">
    <div v-if="error" class="character-alert error" role="alert">
      {{ error }}
    </div>
    <div v-if="notice" class="character-alert" role="status">{{ notice }}</div>
    <section v-if="imported" class="panel character-import">
      <div class="character-heading">
        <div>
          <span class="eyebrow">IMPORT REVIEW</span>
          <h2>确认转换内容</h2>
        </div>
        <button
          class="button secondary small"
          :disabled="!!working"
          @click="imported = null"
        >
          取消
        </button>
      </div>
      <p class="muted">
        {{ imported.card.data.name }} ·
        只导入文本与模块偏好，资源和脚本不会运行。
      </p>
      <p class="character-import-description">
        {{ imported.card.data.description }}
      </p>
      <pre>{{ imported.card.data.system_prompt }}</pre>
      <details>
        <summary>查看完整转换 JSON</summary>
        <pre>{{ JSON.stringify(imported.card, null, 2) }}</pre>
      </details>
      <ul
        v-if="imported.warnings?.length || imported.dropped_fields?.length"
        class="character-warnings"
      >
        <li v-for="(warning, index) in imported.warnings" :key="'w' + index">
          {{ warning.field }}：{{ warning.message }}
        </li>
        <li v-for="field in imported.dropped_fields" :key="field">
          未导入：{{ field }}
        </li>
      </ul>
      <ul v-if="imported.issues?.length" class="character-warnings">
        <li v-for="(issue, index) in imported.issues" :key="index">
          {{ issue.field }}：{{ issue.message }}
        </li>
      </ul>
      <div class="character-actions">
        <button
          class="button secondary"
          :disabled="!!working"
          @click="resetModules(imported.card)"
        >
          改为当前文本模型，关闭视觉与语音</button
        ><button
          class="button primary"
          :disabled="!!working || !!imported.issues?.length"
          @click="confirmImport"
        >
          确认内容，创建草稿
        </button>
      </div>
      <p class="footnote">
        导入确认只创建草稿，不会发布、设为默认或修改全局模型。
      </p>
    </section>
    <div class="character-layout">
      <aside class="panel character-library">
        <div class="character-heading">
          <h2>角色卡库</h2>
          <button
            class="button secondary small"
            :disabled="!!working"
            @click="perform('刷新卡库', refreshLibrary)"
          >
            刷新
          </button>
        </div>
        <p class="muted character-default">
          新会话默认：{{
            library.default
              ? `${library.default.name} v${library.default.revision}`
              : "原有助手设置"
          }}
        </p>
        <p class="footnote">
          QQ 私聊可发送“/角色”查看卡片、“/角色 当前”查看绑定、“/角色 切换
          名称或UUID”开启新角色会话。群聊不开放此管理命令。
        </p>
        <div class="character-actions">
          <button
            class="button primary small"
            :disabled="!!working"
            @click="createCard"
          >
            新建卡片</button
          ><button
            type="button"
            class="button secondary small"
            :disabled="!!working"
            @click="importInput?.click()"
          >
            导入 JSON / ZIP</button
          ><input
            ref="importInput"
            class="character-upload-input"
            type="file"
            tabindex="-1"
            aria-hidden="true"
            accept=".json,.zip,application/json,application/zip"
            :disabled="!!working"
            @change="importFile"
          />
        </div>
        <p v-if="!library.items.length" class="character-empty">
          还没有角色卡。先创建或导入一张，校验后发布。
        </p>
        <button
          v-for="item in library.items"
          :key="item.id"
          class="character-card-button"
          :class="{ active: draft?.card_id === item.id }"
          :disabled="!!working"
          @click="openCard(item)"
        >
          <strong>{{ item.name || "未命名卡片" }}</strong
          ><small
            >{{
              item.archived_at
                ? "已归档"
                : item.latest_revision
                  ? `已发布 v${item.latest_revision}`
                  : "尚未发布"
            }}{{ item.draft ? " · 有草稿" : "" }}</small
          ><span v-if="library.default?.card_id === item.id" class="badge green"
            >新会话默认 v{{ library.default.revision }}</span
          >
        </button>
      </aside>
      <section class="panel character-editor">
        <template v-if="edited && data">
          <div class="character-heading">
            <div>
              <span class="eyebrow">CHARACTER CARD</span>
              <h2>{{ data.name || "未命名角色" }}</h2>
            </div>
            <span class="badge" :class="hasUnsaved ? 'neutral' : 'green'">{{
              hasUnsaved ? "有未保存修改" : "草稿已保存"
            }}</span>
          </div>
          <p class="muted">
            草稿与已发布版本分开保存。修改草稿不影响运行中的角色。
          </p>
          <div v-if="!selectedSummary?.archived_at" class="character-toolbar">
            <div class="character-actions">
              <button
                class="button secondary"
                :disabled="!!working || !hasUnsaved"
                @click="save"
              >
                保存草稿
              </button>
              <button
                class="button secondary"
                :disabled="!!working"
                @click="validate"
              >
                校验卡片
              </button>
              <button
                class="button secondary"
                :disabled="!!working"
                @click="compilePreview"
              >
                编译预览
              </button>
              <button
                class="button secondary"
                :disabled="!!working"
                @click="testConversation"
              >
                独立测试对话
              </button>
              <button
                class="button primary"
                :disabled="!!working || hasUnsaved || !validation?.valid"
                @click="publish"
              >
                发布版本
              </button>
            </div>
            <p class="footnote">
              校验、预览与独立测试会先保存当前草稿；发布版本后才可用于普通新会话。
            </p>
            <p v-if="working" class="footnote" role="status">{{ working }}…</p>
          </div>
          <fieldset
            class="character-fields"
            :disabled="!!working || !!selectedSummary?.archived_at"
          >
            <div class="character-field-row">
              <label>角色名称<input v-model="data.name" maxlength="64" /></label
              ><label
                >昵称<input v-model="data.nickname" maxlength="64"
              /></label>
            </div>
            <label
              >身份与背景<textarea
                v-model="data.description"
                rows="3"
                maxlength="8000"
              ></textarea>
            </label>
            <label
              >稳定性格<textarea
                v-model="data.personality"
                rows="3"
                maxlength="4000"
              ></textarea>
            </label>
            <label
              >对话场景<textarea
                v-model="data.scenario"
                rows="2"
                maxlength="4000"
              ></textarea>
            </label>
            <label
              >人格指令<textarea
                v-model="data.system_prompt"
                rows="5"
                maxlength="8000"
              ></textarea
              ><small>参与人格编译；宿主工具权限仍由服务端控制。</small></label
            >
            <details>
              <summary>示例与开场白</summary>
              <div class="character-detail-fields">
                <label
                  >虚构风格示例<textarea
                    v-model="data.mes_example"
                    rows="5"
                    maxlength="4000"
                  ></textarea
                  ><small>示例不会写入真实对话历史。</small></label
                ><label
                  >首条开场白<textarea
                    v-model="data.first_mes"
                    rows="2"
                    maxlength="2000"
                  ></textarea
                  ><small
                    >仅用于 Web 新会话展示，不会自动向 QQ 发消息。</small
                  ></label
                ><label
                  >备用开场白（每行一条）<textarea
                    v-model="greetings"
                    rows="3"
                  ></textarea></label
                ><label
                  >群聊开场白（首期仅保存）<textarea
                    v-model="groupGreetings"
                    rows="2"
                  ></textarea></label
                ><label
                  >历史后指令（首期不执行）<textarea
                    v-model="data.post_history_instructions"
                    rows="2"
                    maxlength="4000"
                  ></textarea>
                </label>
              </div>
            </details>
            <details open>
              <summary>模块偏好</summary>
              <div class="character-detail-fields">
                <p class="muted">
                  当前仅支持宿主文本模型；视觉与语音待对应功能实现后开放。
                </p>
                <div class="character-field-row">
                  <label
                    >文本模型<AdminSelect
                      :model-value="data.extensions.dsh.modules.chat.mode"
                      aria-label="文本模型"
                      :options="[
                        { value: 'inherit', label: '继承当前模型' },
                        ...(data.extensions.dsh.modules.chat.mode !== 'inherit'
                          ? [
                              {
                                value: data.extensions.dsh.modules.chat.mode,
                                label: '导入的模型偏好（需改为继承）',
                                disabled: true,
                              },
                            ]
                          : []),
                      ]"
                      @update:model-value="
                        data.extensions.dsh.modules.chat = { mode: 'inherit' }
                      " /></label
                  ><label
                    >视觉与语音<input
                      :value="`视觉：${data.extensions.dsh.modules.vision.mode === 'disabled' ? '未启用' : '导入偏好待处理'}；语音：${data.extensions.dsh.modules.speech.mode === 'disabled' ? '未启用' : '导入偏好待处理'}`"
                      readonly
                  /></label>
                </div>
                <button
                  v-if="modulesNeedReset"
                  class="button secondary small"
                  @click="resetModules()"
                >
                  改为当前文本模型，关闭视觉与语音
                </button>
              </div>
            </details>
            <details>
              <summary>分享元数据</summary>
              <div class="character-detail-fields">
                <div class="character-field-row">
                  <label
                    >作者<input v-model="data.creator" maxlength="128" /></label
                  ><label
                    >内容版本标签<input
                      v-model="data.character_version"
                      maxlength="64"
                  /></label>
                </div>
                <label>标签（逗号分隔）<input v-model="tags" /></label
                ><label
                  >作者备注<textarea
                    v-model="data.creator_notes"
                    rows="3"
                    maxlength="4000"
                  ></textarea
                  ><small>仅用于管理展示，不进入模型提示。</small></label
                >
              </div>
            </details>
          </fieldset>
          <div v-if="validation" class="character-validation" role="status">
            <strong>{{
              validation.valid ? "卡片格式校验通过" : "请修复以下问题"
            }}</strong>
            <p v-if="validation.valid && validation.bindable === false">
              卡片可保存，但当前模块偏好未满足运行绑定条件。
            </p>
            <ul>
              <li
                v-for="(issue, index) in [
                  ...validation.errors,
                  ...(validation.warnings || []),
                ]"
                :key="index"
              >
                {{ issue.field }}：{{ issue.message }}
              </li>
            </ul>
          </div>
          <div v-if="preview" class="character-preview">
            <h3>运行人格预览 · {{ preview.bytes }} UTF-8 字节</h3>
            <pre>{{ preview.system_prompt }}</pre>
            <p v-if="preview.ignored_fields?.length" class="footnote">
              未参与编译：{{ preview.ignored_fields.join("、") }}
            </p>
            <ul v-if="preview.warnings?.length" class="character-warnings">
              <li v-for="(warning, index) in preview.warnings" :key="index">
                {{ warning.field }}：{{ warning.message }}
              </li>
            </ul>
          </div>
          <div class="character-published">
            <h3>已发布版本</h3>
            <div v-if="revisions.length" class="character-actions">
              <AdminSelect
                v-model="revisionChoice"
                aria-label="已发布角色版本"
                :options="
                  revisions.map((revision) => ({
                    value: String(revision.revision),
                    label: `v${revision.revision} · ${formatDate(revision.created_at)}`,
                  }))
                "
              /><button
                class="button secondary small"
                :disabled="!!working"
                @click="exportCard"
              >
                导出 JSON</button
              ><button
                class="button secondary small"
                :disabled="!!working || !!selectedSummary?.archived_at"
                @click="editPublishedVersion"
              >
                从此版本建草稿</button
              ><button
                class="button secondary small"
                :disabled="!!working || !!selectedSummary?.archived_at"
                @click="setDefault"
              >
                设为新会话默认</button
              ><button
                class="button secondary small"
                :disabled="!!working || !!selectedSummary?.archived_at"
                @click="archive"
              >
                归档卡片
              </button>
            </div>
            <p v-else class="muted">
              尚未发布。请先校验卡片，再发布不可变版本。
            </p>
            <label
              v-if="selectedSummary?.latest_revision"
              class="character-qq-access"
              ><input
                type="checkbox"
                :checked="!!selectedSummary.qq_selectable"
                :disabled="!!working || !!selectedSummary.archived_at"
                @change="setQqAccess"
              />允许QQ私聊用户选择此角色</label
            >
            <p class="footnote">
              发布不自动共享；QQ开关不影响已有会话或实例默认。新版本和默认值不会改变已有会话的人格。换角色请新建对话。
            </p>
          </div>
        </template>
        <div v-else class="character-empty">
          <h2>让助手拥有熟悉的表达</h2>
          <p>从左侧新建或选择一张角色卡。人格、会话记忆和模型配置分别管理。</p>
        </div>
      </section>
    </div>
  </div>
</template>

<style scoped>
.character-workspace {
  display: grid;
  gap: 18px;
}
.character-layout {
  display: grid;
  grid-template-columns: minmax(220px, 280px) minmax(0, 1fr);
  gap: 22px;
  align-items: start;
}
.character-library,
.character-editor,
.character-import {
  padding: 24px;
  min-width: 0;
}
.character-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 14px;
  margin-bottom: 12px;
}
.character-heading h2 {
  font-size: 16px;
  font-weight: 600;
  overflow-wrap: anywhere;
}
.character-default {
  margin: 12px 0 16px;
  font-size: 12px;
}
.character-actions {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 10px;
  margin-top: 18px;
}
.character-actions .admin-select {
  width: auto;
  min-width: 220px;
  max-width: 100%;
}
.character-upload-input {
  display: none;
}
.character-toolbar {
  margin: 18px 0 22px;
  padding: 16px;
  background: #f7f8fc;
  border: 1px solid var(--line);
  border-radius: 12px;
}
.character-toolbar .character-actions {
  margin: 0 0 10px;
}
.character-toolbar .footnote {
  margin: 8px 0 0;
}
.character-card-button {
  display: flex;
  align-items: flex-start;
  flex-direction: column;
  gap: 5px;
  text-align: left;
  padding: 14px;
  width: 100%;
  margin-top: 12px;
  overflow-wrap: anywhere;
}
.character-card-button.active {
  border-color: var(--indigo);
  background: var(--soft-blue);
}
.character-card-button small {
  color: var(--muted);
}
.character-empty {
  padding: 36px 0;
  color: var(--muted);
  line-height: 1.9;
}
.character-empty h2 {
  margin-bottom: 12px;
  color: var(--ink);
  font-size: 20px;
}
.character-fields {
  padding: 0;
  margin: 20px 0;
  border: 0;
  display: grid;
  gap: 18px;
  min-width: 0;
}
.character-fields label {
  display: grid;
  gap: 7px;
  font-weight: 600;
  font-size: 12px;
  min-width: 0;
}
.character-fields small {
  color: var(--muted);
  font-weight: 400;
  font-size: 12px;
}
.character-fields input,
.character-fields textarea {
  font-size: 14px;
  font-weight: 400;
}
.character-field-row {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}
.character-fields details {
  border-top: 1px solid var(--line);
  padding-top: 16px;
}
.character-fields summary {
  cursor: pointer;
  font-weight: 600;
}
.character-detail-fields {
  margin-top: 18px;
  display: grid;
  gap: 18px;
}
.character-alert {
  padding: 14px 18px;
  border: 1px solid #cbded8;
  border-radius: 12px;
  background: #f0f8f4;
  overflow-wrap: anywhere;
}
.character-alert.error {
  border-color: #e8c2c2;
  background: #fff3f1;
  color: #943d3d;
}
.character-validation,
.character-preview,
.character-published {
  margin-top: 22px;
  padding-top: 20px;
  border-top: 1px solid var(--line);
}
.character-preview h3,
.character-published h3 {
  font-size: 14px;
}
.character-preview pre,
.character-import pre {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  max-height: 360px;
  overflow-y: auto;
  background: var(--canvas);
  padding: 16px;
  border-radius: 10px;
  font: inherit;
}
.character-warnings {
  padding-left: 20px;
  color: #8b612d;
  overflow-wrap: anywhere;
}
.character-import-description {
  margin: 14px 0;
  white-space: pre-wrap;
}
.character-qq-access {
  display: flex;
  align-items: center;
  gap: 9px;
  margin-top: 18px;
  font-size: 13px;
}
.character-qq-access input {
  width: 16px;
  height: 16px;
  padding: 0;
}
@media (max-width: 1000px) {
  .character-layout {
    grid-template-columns: 1fr;
  }
  .character-library {
    display: grid;
    grid-template-columns: 1fr;
  }
}
@media (max-width: 560px) {
  .character-library,
  .character-editor,
  .character-import {
    padding: 18px;
  }
  .character-field-row {
    grid-template-columns: 1fr;
  }
  .character-heading {
    align-items: flex-start;
  }
  .character-actions .button {
    flex: 1 1 auto;
  }
}
</style>
