<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { api, ApiError, errorMessage } from "../api";
import type {
  ContextBudgetPolicy,
  ContextPolicyView,
} from "../context-management";

const emit = defineEmits<{ unauthorized: [] }>();
const snapshot = ref<ContextPolicyView | null>(null);
const form = reactive({
  bytes: 96000,
  estimate: 96000,
  response: 4096,
  protocol: 1024,
  safety: 4096,
  declared: false,
  window: 0,
  bindingConfirmed: false,
});
const original = ref("");
const busy = ref(false),
  error = ref(""),
  notice = ref(""),
  stale = ref(false);
let disposed = false;
let controller: AbortController | undefined;
const hasUnsaved = computed(
  () =>
    busy.value || (!!snapshot.value && JSON.stringify(form) !== original.value),
);
defineExpose({ hasUnsaved });
const validation = computed(() => {
  for (const [value, min, max, label] of [
    [form.bytes, 1, 96000, "请求字节上限"],
    [form.estimate, 1, 96000, "输入估算额度"],
    [form.response, 1, 4096, "响应预留"],
    [form.protocol, 256, 8192, "协议预留"],
    [form.safety, 512, 16384, "安全余量"],
  ] as [number, number, number, string][]) {
    if (!Number.isInteger(value) || value < min || value > max)
      return `${label}须为 ${min}–${max} 的整数。`;
  }
  if (form.declared) {
    if (
      !Number.isInteger(form.window) ||
      form.window > 1048576 ||
      form.window <= form.response + form.protocol + form.safety
    )
      return "声明窗口须大于全部预留，且不超过 1,048,576。";
    if (!form.bindingConfirmed) return "请核对当前模型服务后确认窗口声明。";
  }
  return "";
});
function assign(result: ContextPolicyView) {
  snapshot.value = result;
  const p = result.policy;
  Object.assign(form, {
    bytes: p.max_input_bytes,
    estimate: p.estimated_input_limit,
    response: p.response_reserve_tokens,
    protocol: p.protocol_reserve_tokens,
    safety: p.safety_reserve_tokens,
    declared: p.provider_context_window !== null,
    window: p.provider_context_window ?? 0,
    bindingConfirmed:
      p.provider_context_window !== null && result.provider_binding_matches,
  });
  original.value = JSON.stringify(form);
  stale.value = false;
}
async function load() {
  if (
    busy.value ||
    (hasUnsaved.value &&
      !window.confirm("重新载入会放弃尚未保存的上下文预算修改，确定继续？"))
  )
    return;
  busy.value = true;
  error.value = "";
  notice.value = "";
  controller = new AbortController();
  try {
    const result = await api<ContextPolicyView>("/context-policy", {
      signal: controller.signal,
    });
    if (!disposed) assign(result);
  } catch (caught) {
    if (!disposed) failed(caught);
  } finally {
    if (!disposed) busy.value = false;
  }
}
function failed(caught: unknown) {
  if (caught instanceof ApiError && caught.status === 401) emit("unauthorized");
  if (
    caught instanceof ApiError &&
    (caught.status === 409 ||
      [
        "context_policy_model_mismatch",
        "context_policy_provider_mismatch",
      ].includes(caught.code))
  ) {
    stale.value = true;
    error.value =
      "预算版本、模型或服务已变化。你的修改仍保留，请重新载入最新内容后核对。";
  } else
    error.value =
      caught instanceof ApiError && caught.status === 404
        ? "当前版本尚未开放上下文预算配置。"
        : errorMessage(caught);
}
async function save() {
  if (
    !snapshot.value ||
    busy.value ||
    stale.value ||
    validation.value ||
    !hasUnsaved.value
  )
    return;
  busy.value = true;
  error.value = "";
  notice.value = "";
  controller = new AbortController();
  const current = snapshot.value;
  const body: ContextBudgetPolicy = {
    ...current.policy,
    max_input_bytes: form.bytes,
    estimated_input_limit: form.estimate,
    response_reserve_tokens: form.response,
    protocol_reserve_tokens: form.protocol,
    safety_reserve_tokens: form.safety,
    provider_context_window: form.declared ? form.window : null,
    window_basis: form.declared ? "administrator_declared" : "unknown",
    provider_fingerprint: form.declared
      ? current.current_provider_fingerprint
      : null,
  };
  try {
    const result = await api<ContextPolicyView>("/context-policy", {
      method: "PUT",
      etag: current.etag,
      body,
      signal: controller.signal,
    });
    if (!disposed) {
      assign(result);
      notice.value =
        "上下文预算已保存，将用于新的请求；已冻结的请求继续使用原版本。";
    }
  } catch (caught) {
    if (!disposed) failed(caught);
  } finally {
    if (!disposed) busy.value = false;
  }
}
onMounted(() => {
  void load();
});
onUnmounted(() => {
  disposed = true;
  controller?.abort();
});
</script>

<template>
  <section class="context-policy-panel" aria-label="上下文预算">
    <div class="context-policy-heading">
      <div>
        <h3>上下文预算</h3>
        <p>按完整内容块选择上下文，保留当前问题和必要依据。</p>
      </div>
      <span v-if="snapshot" class="badge neutral"
        >预算 v{{ snapshot.policy.revision }}</span
      >
    </div>
    <p v-if="error" role="alert" class="error-banner">{{ error }}</p>
    <p v-if="notice" role="status" class="notice-banner">{{ notice }}</p>
    <p class="muted">
      当前使用保守估算，不是精确 tokenizer 或 token 计费。请求硬上限 96,000
      字节，响应预留最多 4,096。
    </p>
    <template v-if="snapshot">
      <dl class="context-policy-status">
        <div>
          <dt>当前模型</dt>
          <dd>{{ snapshot.current_model_id }}</dd>
        </div>
        <div>
          <dt>服务窗口</dt>
          <dd>
            {{
              snapshot.window_status === "unknown"
                ? "未知，使用本地估算限制"
                : snapshot.provider_binding_matches
                  ? "管理员声明，尚非自动能力验证"
                  : "服务绑定已变化，须重新核对"
            }}
          </dd>
        </div>
        <div>
          <dt>服务指纹</dt>
          <dd>{{ snapshot.current_provider_fingerprint.slice(0, 12) }}</dd>
        </div>
      </dl>
      <form @submit.prevent="save">
        <fieldset :disabled="busy" class="context-budget-fields">
          <div class="field-row">
            <label class="field"
              ><span>请求字节上限</span
              ><input
                v-model.number="form.bytes"
                aria-label="请求字节上限"
                type="number"
                min="1"
                max="96000"
                required /></label
            ><label class="field"
              ><span>输入估算额度</span
              ><input
                v-model.number="form.estimate"
                aria-label="输入估算额度"
                type="number"
                min="1"
                max="96000"
                required
            /></label>
          </div>
          <div class="field-row">
            <label class="field"
              ><span>响应预留</span
              ><input
                v-model.number="form.response"
                aria-label="响应预留"
                type="number"
                min="1"
                max="4096"
                required /></label
            ><label class="field"
              ><span>协议预留</span
              ><input
                v-model.number="form.protocol"
                aria-label="协议预留"
                type="number"
                min="256"
                max="8192"
                required /></label
            ><label class="field"
              ><span>安全余量</span
              ><input
                v-model.number="form.safety"
                aria-label="安全余量"
                type="number"
                min="512"
                max="16384"
                required
            /></label>
          </div>
          <label class="budget-check"
            ><input
              v-model="form.declared"
              type="checkbox"
              aria-label="声明当前服务窗口"
              @change="form.bindingConfirmed = false"
            />我已从服务商确认当前模型的上下文窗口</label
          >
          <template v-if="form.declared"
            ><label class="field"
              ><span>服务窗口声明</span
              ><input
                v-model.number="form.window"
                aria-label="服务窗口声明"
                type="number"
                min="1"
                max="1048576"
                required /></label
            ><label class="budget-check"
              ><input
                v-model="form.bindingConfirmed"
                type="checkbox"
                aria-label="确认窗口绑定当前服务"
              />将这项声明绑定到上方当前模型与服务指纹</label
            ></template
          >
        </fieldset>
        <p v-if="validation" class="muted" role="status">{{ validation }}</p>
        <div class="context-policy-actions">
          <button
            type="submit"
            class="primary"
            :disabled="busy || stale || !!validation || !hasUnsaved"
          >
            保存上下文预算</button
          ><button type="button" :disabled="busy" @click="load">
            重新载入预算
          </button>
        </div>
      </form>
    </template>
    <button v-else type="button" :disabled="busy" @click="load">
      {{ busy ? "正在读取预算…" : "重新读取预算" }}
    </button>
  </section>
</template>

<style scoped>
.context-policy-panel {
  border-top: 1px solid var(--line);
  margin-top: 24px;
  padding-top: 24px;
}
.context-policy-heading,
.context-policy-actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}
.context-policy-heading h3 {
  margin: 0;
}
.context-policy-heading p {
  margin: 8px 0;
}
.context-policy-panel .muted {
  font-size: 12px;
  line-height: 1.8;
}
.context-policy-status {
  display: grid;
  gap: 8px;
  font-size: 12px;
}
.context-policy-status div {
  display: flex;
  gap: 12px;
}
.context-policy-status dt {
  color: var(--muted);
  min-width: 68px;
}
.context-policy-status dd {
  margin: 0;
  overflow-wrap: anywhere;
}
.context-budget-fields {
  border: 0;
  margin: 0;
  padding: 0;
  min-width: 0;
}
.budget-check {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  font-size: 12px;
  line-height: 1.7;
  margin: 16px 0;
}
.budget-check input {
  flex: none;
  width: auto;
  margin-top: 4px;
}
.context-policy-actions {
  justify-content: flex-start;
  margin-top: 16px;
}
@media (max-width: 650px) {
  .context-policy-panel .field-row {
    display: grid;
    grid-template-columns: 1fr;
  }
}
</style>
