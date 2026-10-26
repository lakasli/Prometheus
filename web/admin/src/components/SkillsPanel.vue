<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { api, ApiError, errorMessage } from "../api";
import type { Skill, SkillDraft, SkillList } from "../types";
import { formatDate } from "../validation";

const props = defineProps<{ initialDraft?: SkillDraft | null }>();
const emit = defineEmits<{
  unauthorized: [];
  use: [id: string];
  consumed: [];
}>();
const items = ref<Skill[]>([]);
const selected = ref<Skill | null>(null);
const form = ref<SkillDraft | null>(null);
const baseline = ref("");
const busy = ref(false);
const error = ref("");
const notice = ref("");
const deleting = ref(false);
const nativeAvailable = ref(false);
const hasUnsaved = computed(
  () => !!form.value && JSON.stringify(form.value) !== baseline.value,
);
const controller = new AbortController();
let disposed = false;
function edit(item?: Skill) {
  if (hasUnsaved.value && !window.confirm("Skill 有未保存的修改，确定切换？"))
    return;
  selected.value = item || null;
  form.value = item
    ? {
        name: item.name,
        description: item.description,
        instructions: item.instructions,
        kind: item.kind,
        enabled: item.enabled,
      }
    : {
        name: "",
        description: "",
        instructions: "",
        kind: "instruction",
        enabled: true,
      };
  baseline.value = JSON.stringify(form.value);
  deleting.value = false;
  notice.value = "";
}
async function load() {
  const result = await api<SkillList>("/skills", { signal: controller.signal });
  if (disposed) return;
  items.value = result.items;
  nativeAvailable.value = result.drawing.native_available;
}
async function action(fn: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  try {
    await fn();
  } catch (caught) {
    if (disposed) return;
    if (caught instanceof ApiError && caught.status === 401)
      emit("unauthorized");
    else error.value = errorMessage(caught);
  } finally {
    busy.value = false;
  }
}
async function save() {
  if (!form.value) return;
  const bytes = (value: string) => new TextEncoder().encode(value).length;
  if (
    !form.value.name.trim() ||
    bytes(form.value.name) > 128 ||
    bytes(form.value.description) > 1024 ||
    !form.value.instructions.trim() ||
    bytes(form.value.instructions) > 16000
  ) {
    error.value =
      "请检查长度：名称 1–128 字节、介绍最多 1,024 字节、指令 1–16,000 字节（中文通常每字 3 字节）。";
    return;
  }
  await action(async () => {
    const result = await api<Skill>(
      selected.value
        ? `/skills/${encodeURIComponent(selected.value.id)}`
        : "/skills",
      {
        method: selected.value ? "PATCH" : "POST",
        body: {
          ...form.value,
          ...(selected.value
            ? { expected_revision: selected.value.revision }
            : {}),
        },
        signal: controller.signal,
      },
    );
    if (disposed) return;
    baseline.value = JSON.stringify(form.value);
    await load();
    edit(items.value.find((item) => item.id === result.id) || result);
    notice.value = "Skill 已保存；新消息使用新版本，已提交任务保持原版本。";
  });
}
async function remove() {
  if (!selected.value) return;
  await action(async () => {
    await api(`/skills/${encodeURIComponent(selected.value!.id)}`, {
      method: "DELETE",
      body: { expected_revision: selected.value!.revision },
      signal: controller.signal,
    });
    if (disposed) return;
    selected.value = null;
    form.value = null;
    baseline.value = "";
    deleting.value = false;
    await load();
    notice.value = "Skill 已删除。历史任务保留使用时的版本。";
  });
}
onMounted(
  () =>
    void action(async () => {
      await load();
      if (props.initialDraft && !disposed) {
        form.value = { ...props.initialDraft, enabled: false };
        baseline.value = "";
        emit("consumed");
        notice.value =
          "这是模型生成的 Skill 草稿，请核对内容后保存；默认停用。";
      }
    }),
);
onUnmounted(() => {
  disposed = true;
  controller.abort();
});
defineExpose({ hasUnsaved });
</script>

<template>
  <div class="skills-workspace">
    <section class="panel skill-library" aria-label="Skill 列表">
      <div class="panel-heading">
        <div>
          <h2>Skill 库</h2>
          <p>让每次任务都按你熟悉的方法完成。</p>
        </div>
        <span class="badge neutral">{{ items.length }} 个</span>
      </div>
      <div class="skill-actions">
        <button class="button primary small" :disabled="busy" @click="edit()">
          新建 Skill</button
        ><button
          class="button secondary small"
          :disabled="busy"
          @click="action(load)"
        >
          刷新列表
        </button>
      </div>
      <p class="muted">
        {{
          nativeAvailable
            ? "绘画：当前模型已配置原生生图。"
            : "绘画：当前使用 HTML / SVG 代码绘制。"
        }}
      </p>
      <button
        v-for="item in items"
        :key="item.id"
        class="skill-item"
        :class="{ active: selected?.id === item.id }"
        :disabled="busy"
        @click="edit(item)"
      >
        <span
          ><strong>{{ item.name }}</strong
          ><small
            >{{ item.builtin ? "默认 Skill · " : "" }}v{{ item.revision }} ·
            {{ item.enabled ? "已启用" : "已停用" }}</small
          ></span
        >
        <p>{{ item.description }}</p>
      </button>
      <p v-if="!items.length && !busy" class="muted">
        还没有 Skill，创建一个开始使用。
      </p>
      <p class="muted">
        Skill 提供任务指令。启用后可在已连接渠道发送 /skill 编号
        任务内容。它不会授予系统、文件或插件权限。
      </p>
    </section>
    <section class="panel skill-editor" aria-label="Skill 详情">
      <p v-if="error" class="skill-error" role="alert">{{ error }}</p>
      <p v-if="notice" role="status">{{ notice }}</p>
      <template v-if="form">
        <div class="panel-heading">
          <div>
            <h2>{{ selected ? "编辑 Skill" : "新建 Skill" }}</h2>
            <small>{{ hasUnsaved ? "有未保存修改" : "内容已保存" }}</small>
          </div>
          <span v-if="selected" class="badge neutral"
            >v{{ selected.revision }}</span
          >
        </div>
        <form @submit.prevent="save">
          <label class="field"
            ><span>Skill 名称</span
            ><input
              v-model="form.name"
              maxlength="80"
              required
              :disabled="busy"
          /></label>
          <label class="field"
            ><span>Skill 介绍</span
            ><textarea
              v-model="form.description"
              rows="3"
              maxlength="1000"
              required
              :disabled="busy"
            ></textarea>
          </label>
          <label class="field"
            ><span>执行方式</span
            ><select v-model="form.kind" :disabled="busy">
              <option value="instruction">任务指令</option>
              <option value="drawing">绘画（原生生图 / HTML）</option>
            </select></label
          >
          <label class="field"
            ><span>Skill 指令</span
            ><textarea
              v-model="form.instructions"
              rows="14"
              maxlength="24000"
              required
              :disabled="busy"
              spellcheck="false"
            ></textarea>
          </label>
          <label class="skill-enabled"
            ><input
              v-model="form.enabled"
              type="checkbox"
              :disabled="busy"
            />启用此 Skill</label
          >
          <div class="skill-actions">
            <button
              class="button primary"
              type="submit"
              :disabled="busy || !hasUnsaved"
            >
              保存 Skill</button
            ><button
              v-if="selected"
              class="button secondary"
              type="button"
              :disabled="busy || hasUnsaved || !selected.enabled"
              @click="notice = `/skill ${selected.id} 任务内容`"
            >
              查看调用命令</button
            ><button
              v-if="selected"
              class="button secondary"
              type="button"
              :disabled="busy"
              @click="deleting = true"
            >
              删除 Skill
            </button>
          </div>
        </form>
        <div v-if="deleting" class="skill-delete" role="alert">
          <p>
            删除「{{ selected?.name }}」后将不能用于新任务，已有任务记录会保留。
          </p>
          <button
            class="button secondary small"
            :disabled="busy"
            @click="remove"
          >
            确认删除 Skill</button
          ><button
            class="button secondary small"
            :disabled="busy"
            @click="deleting = false"
          >
            取消
          </button>
        </div>
        <p v-if="selected" class="muted">
          调用编号：<code>{{ selected.id }}</code
          ><br />最后更新：{{ formatDate(selected.updated_at) }}
        </p>
      </template>
      <div v-else class="skill-empty">
        <h2>添加一种做事方法</h2>
        <p>选择左侧 Skill 查看全部指令，或新建自己的 Skill。</p>
        <p>
          默认的「制作
          Skill」会生成可审核的指令草稿；「绘画」会根据已配置的模型能力生成图片或
          HTML 画面。
        </p>
      </div>
    </section>
  </div>
</template>

<style scoped>
.skills-workspace {
  display: grid;
  grid-template-columns: 280px minmax(0, 1fr);
  gap: 22px;
  align-items: start;
}
.skill-library,
.skill-editor {
  min-width: 0;
}
.skill-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  margin: 14px 0;
}
.skill-item {
  width: 100%;
  text-align: left;
  padding: 16px;
  background: var(--surface, #fff);
  border: 1px solid var(--line, #dce2f3);
  border-radius: 12px;
  color: inherit;
  margin-bottom: 12px;
  cursor: pointer;
}
.skill-item.active {
  background: #eef0fa;
  border-color: #4b5d97;
}
.skill-item strong,
.skill-item small {
  display: block;
}
.skill-item small {
  font-size: 12px;
  margin-top: 6px;
  color: #63749c;
}
.skill-item p {
  font-size: 13px;
  line-height: 1.6;
  margin: 10px 0 0;
  overflow-wrap: anywhere;
}
.skill-editor textarea {
  resize: vertical;
}
.skill-enabled {
  display: flex;
  gap: 8px;
  align-items: center;
  margin: 18px 0;
}
.skill-error {
  color: #a33;
}
.skill-delete {
  padding: 14px;
  background: #fff5f0;
  border-radius: 12px;
}
.skill-delete button {
  margin-right: 8px;
}
.skill-empty {
  padding: 32px 4px;
  line-height: 1.8;
}
.muted {
  font-size: 12px;
  line-height: 1.8;
  overflow-wrap: anywhere;
}
select {
  padding: 12px;
  border: 1px solid #dce2f3;
  border-radius: 10px;
  background: #fff;
  color: inherit;
  width: 100%;
}
@media (max-width: 1000px) {
  .skills-workspace {
    grid-template-columns: 230px minmax(0, 1fr);
  }
}
@media (max-width: 760px) {
  .skills-workspace {
    grid-template-columns: minmax(0, 1fr);
  }
  .skill-actions .button {
    max-width: 100%;
  }
}
</style>
