<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { api } from "../api";
import type { Personality, PersonalityState } from "../types";
const props = defineProps<{
  current: PersonalityState;
  dirty: boolean;
  disabled: boolean;
}>();
const emit = defineEmits<{ restored: [PersonalityState]; error: [unknown] }>();
const history = ref<
  { revision: number; created_at: number; personality: Personality }[]
>([]);
const selected = ref(0),
  busy = ref(false);
async function load() {
  try {
    const data = await api<{ items: typeof history.value }>(
      "/assistant/personality/history",
    );
    history.value = data.items.filter(
      (item) => item.revision !== props.current.revision,
    );
    if (!history.value.some((item) => item.revision === selected.value))
      selected.value = history.value[0]?.revision ?? 0;
  } catch (error) {
    emit("error", error);
  }
}
async function restore() {
  const item = history.value.find((item) => item.revision === selected.value);
  if (
    !item ||
    !window.confirm(
      `将历史性格 v${item.revision} 保存为新版本${props.dirty ? "，并放弃尚未保存的编辑" : ""}，确认恢复？`,
    )
  )
    return;
  const expected = props.current.revision;
  busy.value = true;
  try {
    const saved = await api<PersonalityState>(
      "/assistant/personality",
      "PUT",
      { personality: item.personality },
      expected,
    );
    emit("restored", saved);
  } catch (error) {
    emit("error", error);
  } finally {
    busy.value = false;
  }
}
watch(() => props.current.revision, load);
onMounted(load);
</script>
<template>
  <details class="feature-section">
    <summary>性格版本与恢复</summary>
    <p class="muted">
      恢复会另存为新版本，用于下一次运行；稳定身份、记忆和模型配置保留。
    </p>
    <div v-if="history.length" class="form-row">
      <label
        >历史性格版本<select
          v-model.number="selected"
          aria-label="历史性格版本"
          :disabled="busy || disabled"
        >
          <option
            v-for="item in history"
            :key="item.revision"
            :value="item.revision"
          >
            v{{ item.revision }} ·
            {{
              new Date(item.created_at * 1000).toLocaleString("zh-CN", {
                timeZone: "Asia/Shanghai",
              })
            }}
          </option>
        </select></label
      >
      <button
        class="secondary"
        :disabled="busy || disabled || !selected"
        @click="restore"
      >
        恢复此版本
      </button>
    </div>
    <p v-else class="muted">尚无可恢复的旧版本。</p>
  </details>
</template>
