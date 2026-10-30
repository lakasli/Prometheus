<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>(),
  state = ref<any>(null),
  confirmed = ref(false),
  declared = ref(false),
  windowSize = ref(64000),
  busy = ref(false),
  notice = ref("");
async function load() {
  try {
    state.value = await api("/context/policy");
    declared.value = state.value.value.provider_context_window !== null;
    windowSize.value = state.value.value.provider_context_window ?? 64000;
    confirmed.value =
      declared.value &&
      state.value.value.provider_fingerprint ===
        state.value.current_provider_fingerprint;
  } catch (error) {
    emit("error", error);
  }
}
async function save() {
  busy.value = true;
  try {
    if (declared.value && !confirmed.value)
      throw new Error("请先核对当前模型服务，再确认窗口声明。");
    const value = {
      ...state.value.value,
      provider_context_window: declared.value ? windowSize.value : null,
      provider_fingerprint: declared.value
        ? state.value.current_provider_fingerprint
        : null,
      window_basis: declared.value ? "administrator_declared" : "unknown",
    };
    await api("/context/policy", "PUT", value, state.value.revision);
    await load();
    notice.value = "上下文预算已保存。估算不是精确的token计数。";
  } catch (error) {
    emit("error", error);
  } finally {
    busy.value = false;
  }
}
onMounted(load);
</script>
<template>
  <section v-if="state" class="panel feature-section form-panel">
    <h2>上下文预算</h2>
    <p class="muted">
      保留必要输入，按完整轮次选择历史；供应商窗口未知时明确保持未知。
    </p>
    <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
    <div class="form-row">
      <label
        >请求字节上限<input
          v-model.number="state.value.max_input_bytes"
          type="number"
          min="1"
          max="96000" /></label
      ><label
        >输入估算额度<input
          v-model.number="state.value.estimated_input_limit"
          type="number"
          min="1"
          max="96000" /></label
      ><label
        >响应预留<input
          v-model.number="state.value.response_reserve_tokens"
          type="number"
          min="1"
          max="4096"
      /></label>
    </div>
    <div class="form-row">
      <label
        >协议预留<input
          v-model.number="state.value.protocol_reserve_tokens"
          type="number"
          min="256"
          max="8192" /></label
      ><label
        >安全余量<input
          v-model.number="state.value.safety_reserve_tokens"
          type="number"
          min="512"
          max="16384"
      /></label>
    </div>
    <label class="checkbox"
      ><input v-model="declared" type="checkbox" />声明已核对的模型窗口</label
    ><template v-if="declared"
      ><label
        >声明窗口<input
          v-model.number="windowSize"
          type="number"
          min="1"
          max="1048576" /></label
      ><label class="checkbox"
        ><input
          v-model="confirmed"
          type="checkbox"
        />已核对当前服务与模型，确认上述窗口</label
      ></template
    ><button class="primary" :disabled="busy" @click="save">
      保存上下文预算
    </button>
  </section>
</template>
