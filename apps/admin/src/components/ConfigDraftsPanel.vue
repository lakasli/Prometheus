<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { api } from "../api";
import type { Config, ConfigState } from "../types";
interface Validation {
  valid: boolean;
  errors: { code: string; message: string }[];
  warnings: string[];
  draft_revision: number;
}
interface Draft {
  id: string;
  revision: number;
  base_revision: number;
  state: string;
  config: Config;
  stale_base: boolean;
  current_revision: number;
  applied_revision: number | null;
  is_current_revision: boolean;
  validation: Validation | null;
  last_check_id: string | null;
  applied_operation_id: string | null;
  differences: { field: string; current: unknown; draft: unknown }[];
}
interface Operation {
  id: string;
  kind: string;
  status: string;
  draft_id: string | null;
  draft_revision: number | null;
  error: string | null;
  result: {
    ok?: boolean;
    applied_revision?: number;
    checks?: { name: string; ok: boolean; message: string }[];
  } | null;
}
const props = defineProps<{
  current: ConfigState;
  working: Config;
  dirty: boolean;
  pendingSecret: boolean;
  disabled: boolean;
}>();
const emit = defineEmits<{
  loaded: [Config];
  applied: [];
  working: [boolean];
  error: [unknown];
}>();
const drafts = ref<Draft[]>([]),
  operations = ref<Operation[]>([]),
  selected = ref(""),
  selectedOperation = ref(""),
  busy = ref(false),
  notice = ref("");
const chosen = computed(() =>
  drafts.value.find((item) => item.id === selected.value),
);
const operation = computed(() =>
  operations.value.find((item) => item.id === selectedOperation.value),
);
const valid = computed(
  () =>
    chosen.value?.validation?.valid &&
    chosen.value.validation.draft_revision === chosen.value.revision,
);
const checking = computed(() =>
  operations.value.some((item) =>
    ["running", "cancelling"].includes(item.status),
  ),
);
let timer: ReturnType<typeof setTimeout> | undefined,
  generation = 0,
  disposed = false;
const controller = new AbortController();
const fieldNames: Record<string, string> = {
  "model.base_url": "模型服务",
  "model.model": "模型名称",
  "model.credential_ref": "模型凭据引用",
  "runtime.max_concurrent_runs": "并发上限",
  "runtime.timeout_seconds": "请求超时",
  "runtime.max_history_messages": "历史消息数量",
  "qq.enabled": "QQ开关",
  "qq.app_id": "QQ App ID",
  "qq.credential_ref": "QQ凭据引用",
  "qq.transport": "QQ接入方式",
  "weixin.enabled": "微信开关",
  "speech.enabled": "语音开关",
};
function valueText(value: unknown) {
  return value === null
    ? "尚未选择"
    : value === true
      ? "启用"
      : value === false
        ? "停用"
        : String(value || "空");
}
function statusText(item: Operation) {
  return (
    (
      {
        running: "检查中",
        cancelling: "正在取消",
        cancelled: "已取消",
        interrupted: "已中断",
        invalidated: "旧检查已失效",
        failed: "检查未通过",
        completed: item.kind === "apply" ? "应用完成" : "检查完成",
      } as Record<string, string>
    )[item.status] ?? "结果待确认"
  );
}
async function load() {
  const version = ++generation;
  const [a, b] = await Promise.all([
    api<{ items: Draft[] }>(
      "/config/drafts",
      "GET",
      undefined,
      undefined,
      controller.signal,
    ),
    api<{ items: Operation[] }>(
      "/config/operations",
      "GET",
      undefined,
      undefined,
      controller.signal,
    ),
  ]);
  if (disposed || version !== generation) return;
  drafts.value = a.items;
  operations.value = b.items;
  if (
    selected.value &&
    !drafts.value.some((item) => item.id === selected.value)
  )
    selected.value = "";
  if (
    selectedOperation.value &&
    !operations.value.some((item) => item.id === selectedOperation.value)
  )
    selectedOperation.value = "";
  if (timer) clearTimeout(timer);
  if (checking.value)
    timer = setTimeout(() => {
      void load().catch((error) => {
        if (!disposed) emit("error", error);
      });
    }, 400);
}
async function action(run: () => Promise<void>) {
  if (busy.value || props.disabled) return;
  busy.value = true;
  emit("working", true);
  notice.value = "";
  try {
    await run();
  } catch (error) {
    emit("error", error);
  } finally {
    busy.value = false;
    emit("working", false);
  }
}
async function save(update = false) {
  if (props.pendingSecret) return;
  const item = chosen.value;
  if (
    update &&
    (!item ||
      !window.confirm("使用当前表单替换选中的草稿？只保存草稿，不立即生效。"))
  )
    return;
  await action(async () => {
    const saved =
      update && item
        ? await api<Draft>(
            `/config/drafts/${item.id}`,
            "PUT",
            { config: props.working },
            item.revision,
          )
        : await api<Draft>("/config/drafts", "POST", {
            base_revision: props.current.revision,
            config: props.working,
          });
    selected.value = saved.id;
    await load();
    notice.value = "草稿已保存，当前生效配置未变化。";
  });
}
function useDraft() {
  const item = chosen.value;
  if (!item) return;
  if (
    props.dirty &&
    !window.confirm(
      "载入草稿会放弃当前表单编辑和尚未加密保存的凭据，是否继续？",
    )
  )
    return;
  emit("loaded", JSON.parse(JSON.stringify(item.config)) as Config);
  notice.value = "已载入表单，尚未应用。";
}
async function validate() {
  const item = chosen.value;
  if (!item) return;
  await action(async () => {
    await api(`/config/drafts/${item.id}/validate`, "POST", {}, item.revision);
    await load();
  });
}
async function check() {
  const item = chosen.value;
  if (!item) return;
  await action(async () => {
    const result = await api<Operation>(
      `/config/drafts/${item.id}/checks`,
      "POST",
      {},
      item.revision,
    );
    selectedOperation.value = result.id;
    await load();
    notice.value = "连接检查已开始，配置尚未应用。";
  });
}
async function cancel() {
  const item = operation.value;
  if (!item) return;
  await action(async () => {
    await api(`/config/operations/${item.id}`, "DELETE");
    await load();
    notice.value = "已请求取消；连接关闭前仍占用请求槽。";
  });
}
async function rebase() {
  const item = chosen.value;
  if (
    !item ||
    !window.confirm(
      `已核对下方差异？此操作保留草稿全部字段，以当前配置 v${item.current_revision} 为基准；尚不生效。`,
    )
  )
    return;
  await action(async () => {
    await api(
      `/config/drafts/${item.id}/rebase`,
      "POST",
      { base_revision: item.current_revision, confirmed: true },
      item.revision,
    );
    await load();
    notice.value = "草稿已重新定位，请重新校验并核对连接。";
  });
}
async function apply() {
  const item = chosen.value;
  if (!item || props.pendingSecret) return;
  const changed = JSON.stringify(props.working) !== JSON.stringify(item.config);
  if (
    !window.confirm(
      `确认应用已保存的草稿 v${item.revision}？${changed ? "当前表单未保存的修改将被放弃。" : ""}连接检查与应用是独立步骤，请核对检查结果。`,
    )
  )
    return;
  let applied = false;
  await action(async () => {
    const result = await api<Operation>(
      `/config/drafts/${item.id}/apply`,
      "POST",
      { base_revision: item.base_revision, confirmed: true },
      item.revision,
    );
    selectedOperation.value = result.id;
    await load();
    notice.value = `配置已应用为 v${result.result?.applied_revision}；操作回执已保存。`;
    applied = true;
  });
  if (applied) emit("applied");
}
async function remove() {
  const item = chosen.value;
  if (!item || !window.confirm("删除这个草稿？已生效配置和操作回执保留。"))
    return;
  await action(async () => {
    await api(`/config/drafts/${item.id}`, "DELETE", undefined, item.revision);
    await load();
    notice.value = "草稿已删除。";
  });
}
watch(
  () => props.current.revision,
  () => {
    void load().catch((error) => {
      if (!disposed) emit("error", error);
    });
  },
);
onMounted(() => {
  void load().catch((error) => {
    if (!disposed) emit("error", error);
  });
});
onUnmounted(() => {
  disposed = true;
  controller.abort();
  if (timer) clearTimeout(timer);
});
</script>
<template>
  <section class="panel configuration-drafts">
    <details>
      <summary>配置草稿与操作记录</summary>
      <p class="muted">
        草稿只保存编辑内容。字段校验、连接检查、明确应用分别进行；最多保留32个草稿，密钥需先在上方单独加密保存。
      </p>
      <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
      <p v-if="pendingSecret" class="info-note">
        有尚未加密保存的凭据，请先保存或清空，再保存草稿或应用。
      </p>
      <div class="form-actions">
        <button
          class="secondary"
          :disabled="busy || disabled || pendingSecret"
          @click="save(false)"
        >
          另存当前表单为草稿</button
        ><button
          class="secondary"
          :disabled="busy || disabled"
          @click="action(load)"
        >
          刷新草稿与操作
        </button>
      </div>
      <label
        >选择配置草稿<select v-model="selected" aria-label="选择配置草稿">
          <option value="">请选择</option>
          <option v-for="item in drafts" :key="item.id" :value="item.id">
            {{ item.id.slice(0, 8) }} · 草稿v{{ item.revision }} · 基准v{{
              item.base_revision
            }}
            ·
            {{
              item.state === "applied"
                ? `曾应用v${item.applied_revision}`
                : "待应用"
            }}
          </option>
        </select></label
      >
      <template v-if="chosen">
        <p class="muted">
          草稿内容版本 v{{ chosen.revision }} ·
          {{
            chosen.is_current_revision
              ? `当前生效 v${chosen.applied_revision}`
              : chosen.state === "applied"
                ? `曾应用 v${chosen.applied_revision}`
                : "已保存，未生效"
          }}
        </p>
        <p v-if="chosen.stale_base" class="info-note">
          生效配置已变化，旧基准不能直接应用。请核对全部差异后重新定位，或重新新建草稿。
        </p>
        <ul class="draft-differences">
          <li v-for="item in chosen.differences" :key="item.field">
            {{ fieldNames[item.field] || item.field }}：{{
              valueText(item.current)
            }}
            → {{ valueText(item.draft) }}
          </li>
        </ul>
        <div class="form-actions">
          <button
            class="secondary"
            :disabled="busy || disabled"
            @click="useDraft"
          >
            载入草稿到表单</button
          ><button
            class="secondary"
            :disabled="
              busy || disabled || pendingSecret || chosen.state !== 'draft'
            "
            @click="save(true)"
          >
            用当前表单更新草稿</button
          ><button
            class="text-button danger"
            :disabled="busy || disabled"
            @click="remove"
          >
            删除所选草稿
          </button>
        </div>
        <div v-if="chosen.state === 'draft'" class="form-actions">
          <button
            class="secondary"
            :disabled="busy || disabled"
            @click="validate"
          >
            校验草稿</button
          ><button
            class="secondary"
            :disabled="busy || disabled || !valid || checking || pendingSecret"
            @click="check"
          >
            检查草稿连接</button
          ><button
            class="primary"
            :disabled="
              busy ||
              disabled ||
              !valid ||
              checking ||
              pendingSecret ||
              chosen.stale_base
            "
            @click="apply"
          >
            应用已保存草稿</button
          ><button
            v-if="chosen.stale_base"
            class="secondary"
            :disabled="busy || disabled"
            @click="rebase"
          >
            核对差异并重新定位草稿
          </button>
        </div>
        <div v-if="chosen.validation">
          <p>
            {{
              chosen.validation.valid
                ? "字段校验通过；连接状态以操作记录为准。"
                : "字段校验未通过。"
            }}
          </p>
          <p
            v-for="item in chosen.validation.errors"
            :key="item.code"
            class="error"
          >
            {{ item.message }}
          </p>
          <p
            v-for="item in chosen.validation.warnings"
            :key="item"
            class="muted"
          >
            {{ item }}
          </p>
        </div>
      </template>
      <h3>操作记录</h3>
      <p class="muted">
        记录属于当前管理账户；检查结果不会自动应用，服务中断的检查不自动重试。
      </p>
      <label
        >查看配置操作<select
          v-model="selectedOperation"
          aria-label="查看配置操作"
        >
          <option value="">请选择</option>
          <option v-for="item in operations" :key="item.id" :value="item.id">
            {{ item.id.slice(0, 8) }} ·
            {{ item.kind === "apply" ? "应用" : "连接检查" }} ·
            {{ statusText(item) }}
          </option>
        </select></label
      >
      <article v-if="operation" class="draft-operation">
        <strong>{{ statusText(operation) }}</strong>
        <p v-if="operation.error" class="error">{{ operation.error }}</p>
        <p v-for="check in operation.result?.checks" :key="check.name">
          {{ check.name === "qq" ? "QQ" : "模型" }}：{{
            check.ok ? "通过" : "未通过"
          }}
          · {{ check.message }}
        </p>
        <button
          v-if="['running', 'cancelling'].includes(operation.status)"
          class="secondary"
          :disabled="busy || disabled || operation.status === 'cancelling'"
          @click="cancel"
        >
          取消连接检查
        </button>
        <details>
          <summary>操作回执</summary>
          <pre>{{ JSON.stringify(operation, null, 2) }}</pre>
        </details>
      </article>
    </details>
  </section>
</template>
