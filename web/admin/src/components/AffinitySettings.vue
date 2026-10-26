<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { api, ApiError, errorMessage } from "../api";

const emit = defineEmits<{ unauthorized: [] }>();

interface AffinityPolicy {
  revision: number;
  initial: number;
  minimum: number;
  maximum: number;
  daily_decay: number;
  grace_seconds: number;
  downtime_cap_seconds: number;
  effective_at: number;
}
const loadedPolicy = ref<AffinityPolicy | null>(null);
const policyForm = reactive({
  initial: 50,
  minimum: 0,
  maximum: 100,
  daily_decay: 1,
  grace_seconds: 259200,
  downtime_cap_seconds: 604800,
  confirmed: false,
  reason: "",
});
const previewDays = ref(10);
const policyPreview = ref<number | null>(null);
const busy = ref(false);
const error = ref("");
const notice = ref("");
const savedForm = ref("");
let disposed = false;
const hasUnsaved = computed(
  () =>
    busy.value ||
    (Boolean(loadedPolicy.value) &&
      JSON.stringify(policyForm) !== savedForm.value),
);
defineExpose({ hasUnsaved });

async function perform(operation: () => Promise<unknown>, message: string) {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  notice.value = "";
  try {
    await operation();
    if (!disposed) notice.value = message;
  } catch (caught) {
    if (!disposed) {
      error.value = errorMessage(caught);
      if (caught instanceof ApiError && caught.status === 401)
        emit("unauthorized");
    }
  } finally {
    if (!disposed) busy.value = false;
  }
}
async function loadPolicy() {
  if (
    hasUnsaved.value &&
    !busy.value &&
    !window.confirm("好感规则有未保存的修改，确定重新载入？")
  )
    return;
  await perform(async () => {
    const policy = await api<AffinityPolicy>("/relationships/affinity-policy");
    if (disposed) return;
    loadedPolicy.value = policy;
    Object.assign(policyForm, {
      initial: policy.initial,
      minimum: policy.minimum,
      maximum: policy.maximum,
      daily_decay: policy.daily_decay,
      grace_seconds: policy.grace_seconds,
      downtime_cap_seconds: policy.downtime_cap_seconds,
      confirmed: false,
      reason: "",
    });
    savedForm.value = JSON.stringify(policyForm);
    policyPreview.value = null;
  }, "当前好感规则已载入。");
}
async function previewPolicy() {
  if (!policyForm.reason.trim()) {
    error.value = "请先填写规则调整依据。";
    return;
  }
  await perform(async () => {
    const result = await api<{ projected_from_initial: number }>(
      "/relationships/affinity-policy/preview",
      {
        method: "POST",
        body: {
          policy: policyForm,
          idle_seconds: Math.round(previewDays.value * 86400),
        },
      },
    );
    if (!disposed) policyPreview.value = result.projected_from_initial;
  }, "预演完成，未修改实际分数或联系许可。");
}
async function savePolicy() {
  if (
    !loadedPolicy.value ||
    !policyForm.confirmed ||
    !policyForm.reason.trim()
  ) {
    error.value = "规则变更须填写依据并明确确认。";
    return;
  }
  const submitted = { ...policyForm };
  await perform(async () => {
    const result = await api<AffinityPolicy>("/relationships/affinity-policy", {
      method: "PUT",
      revision: loadedPolicy.value!.revision,
      body: submitted,
    });
    if (disposed) return;
    loadedPolicy.value = result;
    policyForm.confirmed = false;
    savedForm.value = JSON.stringify({ ...submitted, confirmed: false });
  }, "新规则已生效，旧规则结算区间和历史版本保留。");
}
onMounted(loadPolicy);
onUnmounted(() => {
  disposed = true;
});
</script>

<template>
  <section class="affinity-settings">
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <p v-if="notice" class="success" role="status">{{ notice }}</p>
    <button type="button" :disabled="busy" @click="loadPolicy">
      {{ loadedPolicy ? "重新载入好感规则" : "载入好感规则" }}
    </button>
    <form
      v-if="loadedPolicy"
      class="affinity-form"
      @submit.prevent="savePolicy"
    >
      <p>
        当前版本 v{{
          loadedPolicy.revision
        }}。新规则只作用于生效水位之后；好感不会改变基础回复、纠错、隐私权或接收许可。
      </p>
      <fieldset :disabled="busy">
        <label>
          新档案初始分
          <input
            v-model.number="policyForm.initial"
            type="number"
            min="0"
            max="100"
            required
          />
        </label>
        <label>
          下界
          <input
            v-model.number="policyForm.minimum"
            type="number"
            min="0"
            max="99"
            required
          />
        </label>
        <label>
          上界
          <input
            v-model.number="policyForm.maximum"
            type="number"
            min="1"
            max="100"
            required
          />
        </label>
        <label>
          每日空闲衰减
          <input
            v-model.number="policyForm.daily_decay"
            type="number"
            min="0"
            max="10"
            step="0.001"
            required
          />
        </label>
        <label>
          直接互动后宽限秒数
          <input
            v-model.number="policyForm.grace_seconds"
            type="number"
            min="0"
            max="2592000"
            required
          />
        </label>
        <label>
          未观测停机区间最多补算秒数
          <input
            v-model.number="policyForm.downtime_cap_seconds"
            type="number"
            min="0"
            max="2592000"
            required
          />
        </label>
        <label class="full-width">
          调整依据
          <textarea v-model="policyForm.reason" required />
        </label>
        <label>
          预演空闲天数
          <input
            v-model.number="previewDays"
            type="number"
            min="0"
            max="3650"
            step="0.1"
          />
        </label>
      </fieldset>
      <button type="button" :disabled="busy" @click="previewPolicy">
        预演分数
      </button>
      <p v-if="policyPreview !== null" role="status">
        从初始分出发的预演结果：{{
          policyPreview.toFixed(3)
        }}。本次预演不改实际分数。
      </p>
      <label class="checkbox">
        <input
          v-model="policyForm.confirmed"
          :disabled="busy"
          type="checkbox"
        />
        明确确认新规则从生效水位向后应用
      </label>
      <button :disabled="busy" type="submit">保存好感规则</button>
    </form>
  </section>
</template>

<style scoped>
.affinity-settings,
.affinity-form {
  display: grid;
  gap: 16px;
}
.affinity-settings > button,
.affinity-form > button {
  justify-self: start;
}
.affinity-form p {
  margin: 0;
  color: var(--muted);
}
fieldset {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 18px;
  padding: 0;
  margin: 0;
  border: 0;
  min-width: 0;
}
label {
  display: grid;
  gap: 6px;
}
.full-width {
  grid-column: 1 / -1;
}
input:not([type="checkbox"]),
textarea {
  width: 100%;
  box-sizing: border-box;
}
textarea {
  min-height: 70px;
}
.checkbox {
  display: flex;
  align-items: center;
  gap: 8px;
}
.error {
  color: #9e3832;
}
.success {
  color: #3d663f;
}
@media (max-width: 640px) {
  fieldset {
    grid-template-columns: 1fr;
  }
}
</style>
