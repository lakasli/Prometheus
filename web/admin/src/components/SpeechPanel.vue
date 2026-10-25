<script setup lang="ts">
import { computed, onMounted, ref, useId } from "vue";
import { api, errorMessage } from "../api";

interface SpeechConfig {
  enabled: boolean;
  voice: string;
  timeout_seconds: number;
  max_text_chars: number;
}
interface ConfigView {
  revision: number;
  applied_revision: number;
  config: Record<string, unknown> & { speech: SpeechConfig };
}
interface Health {
  enabled: boolean;
  available: boolean;
  runtime_present: boolean;
  active_workers: number;
  max_workers: number;
  voice: string;
}
interface Asset {
  id: string;
  audio_url: string;
  duration_ms: number;
  synthesis_ms: number;
  bytes: number;
}
const id = useId();
const config = ref<ConfigView | null>(null);
const speech = ref<SpeechConfig>({
  enabled: false,
  voice: "zh_CN-huayan-medium",
  timeout_seconds: 30,
  max_text_chars: 300,
});
const health = ref<Health | null>(null);
const sample = ref("你好，这是在树莓派上本地合成的语音。");
const asset = ref<Asset | null>(null);
const busy = ref(false);
const error = ref("");
const notice = ref("");
const valid = computed(
  () =>
    speech.value.timeout_seconds >= 5 &&
    speech.value.timeout_seconds <= 60 &&
    speech.value.max_text_chars >= 1 &&
    speech.value.max_text_chars <= 400,
);
const sampleLength = computed(() => [...sample.value].length);

async function load() {
  busy.value = true;
  error.value = "";
  try {
    const [settings, state] = await Promise.all([
      api<ConfigView>("/config"),
      api<Health>("/speech"),
    ]);
    config.value = settings;
    speech.value = { ...settings.config.speech };
    health.value = state;
  } catch (cause) {
    error.value = errorMessage(cause);
  } finally {
    busy.value = false;
  }
}
async function save() {
  if (!config.value || !valid.value) return;
  busy.value = true;
  error.value = notice.value = "";
  try {
    const draft = await api<{ id: string }>("/config/drafts", {
      method: "POST",
      body: {
        base_revision: config.value.revision,
        config: { ...config.value.config, speech: { ...speech.value } },
      },
    });
    const validation = await api<{
      valid: boolean;
      errors: { message: string }[];
    }>(`/config/drafts/${draft.id}/validate`, { method: "POST", body: {} });
    if (!validation.valid) {
      error.value = validation.errors.map((item) => item.message).join("；");
      return;
    }
    const applied = await api<{
      desired_revision: number;
      applied_revision: number;
    }>(`/config/drafts/${draft.id}/apply`, {
      method: "POST",
      revision: config.value.revision,
      body: {},
    });
    await load();
    notice.value =
      applied.desired_revision === applied.applied_revision
        ? `语音设置已保存并生效（版本 ${applied.applied_revision}），无需重启。`
        : "语音设置已保存，等待应用。";
  } catch (cause) {
    error.value = errorMessage(cause);
  } finally {
    busy.value = false;
  }
}
async function synthesize() {
  if (
    busy.value ||
    !sampleLength.value ||
    sampleLength.value > speech.value.max_text_chars
  )
    return;
  busy.value = true;
  error.value = notice.value = "";
  try {
    asset.value = await api<Asset>("/speech/syntheses", {
      method: "POST",
      body: { text: sample.value },
    });
    health.value = await api<Health>("/speech");
    notice.value =
      "本地合成完成。点击播放试听；QQ 发送结果请在渠道会话中核验。";
  } catch (cause) {
    error.value = errorMessage(cause);
  } finally {
    busy.value = false;
  }
}
async function removeAudio() {
  if (!asset.value || busy.value) return;
  busy.value = true;
  error.value = "";
  try {
    await api(`/speech/assets/${asset.value.id}`, { method: "DELETE" });
    asset.value = null;
    notice.value = "试听音频已清理。";
  } catch (cause) {
    error.value = errorMessage(cause);
  } finally {
    busy.value = false;
  }
}
onMounted(load);
</script>

<template>
  <section class="speech-panel" aria-label="本地语音合成">
    <div class="section-heading">
      <div>
        <h2>本地语音</h2>
        <p>Piper 在树莓派上合成中文语音，优先用于 QQ 私聊语音回复。</p>
      </div>
      <button class="button secondary" :disabled="busy" @click="load">刷新状态</button>
    </div>
    <p v-if="error" role="alert" class="error-message">{{ error }}</p>
    <p v-if="notice" role="status" class="notice">{{ notice }}</p>
    <div v-if="health" class="speech-status">
      <span>{{
        health.enabled
          ? health.available
            ? "可用"
            : "已启用，运行时不可用"
          : "未启用"
      }}</span>
      <span>声音：{{ health.voice }}</span
      ><span
        >活跃 worker：{{ health.active_workers }} /
        {{ health.max_workers }}</span
      >
    </div>
    <form @submit.prevent="save">
      <label class="speech-switch"
        ><input
          v-model="speech.enabled"
          type="checkbox"
          :disabled="busy"
        />启用本地 Piper TTS</label
      >
      <div class="speech-fields">
        <label :for="`${id}-timeout`"
          >合成超时（秒）<input
            :id="`${id}-timeout`"
            v-model.number="speech.timeout_seconds"
            type="number"
            min="5"
            max="60"
            required
            :disabled="busy"
        /></label>
        <label :for="`${id}-max`"
          >每段最多字符<input
            :id="`${id}-max`"
            v-model.number="speech.max_text_chars"
            type="number"
            min="1"
            max="400"
            required
            :disabled="busy"
        /></label>
      </div>
      <p class="muted">
        固定中文声音，本地数据路径，单个按需
        worker；合成失败时聊天保留文字。配置可通过版本记录回退。
      </p>
      <button class="button" type="submit" :disabled="busy || !config || !valid">
        保存并生效
      </button>
    </form>
    <div class="speech-preview">
      <label :for="`${id}-sample`">试听文字</label>
      <textarea
        :id="`${id}-sample`"
        v-model="sample"
        rows="3"
        :disabled="busy"
      />
      <p class="muted">
        {{ sampleLength }} /
        {{
          speech.max_text_chars
        }}
        字符。仅合成普通文字；试听音频私有保存最多一小时。
      </p>
      <button class="button"
        :disabled="
          busy ||
          !health?.available ||
          !sampleLength ||
          sampleLength > speech.max_text_chars
        "
        @click="synthesize"
      >
        {{ busy ? "处理中…" : "合成试听" }}
      </button>
      <div v-if="asset" class="speech-result">
        <audio :key="asset.id" controls preload="none" :src="asset.audio_url">
          浏览器不支持音频播放。
        </audio>
        <span
          >时长 {{ (asset.duration_ms / 1000).toFixed(1) }} 秒 · 合成
          {{ (asset.synthesis_ms / 1000).toFixed(1) }} 秒</span
        >
        <button class="button secondary" :disabled="busy" @click="removeAudio">
          清理试听音频
        </button>
      </div>
    </div>
  </section>
</template>

<style scoped>
.speech-panel {
  display: grid;
  gap: 1.2rem;
}
.section-heading {
  display: flex;
  justify-content: space-between;
  gap: 1rem;
  align-items: start;
}
.section-heading h2 {
  margin-top: 0;
}
.speech-status,
.speech-fields,
.speech-result {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 1rem;
}
.speech-fields label {
  display: grid;
  gap: 0.35rem;
  min-width: 12rem;
}
.speech-switch {
  display: flex;
  gap: 0.6rem;
  align-items: center;
  margin-bottom: 1rem;
}
.speech-switch input {
  width: auto;
}
.speech-preview {
  display: grid;
  gap: 0.7rem;
  padding-top: 1rem;
  border-top: 1px solid var(--border, #d4dbe4);
}
.speech-preview textarea {
  width: 100%;
  box-sizing: border-box;
}
.speech-preview > button {
  justify-self: start;
}
.speech-result audio {
  max-width: 100%;
}
.muted {
  color: var(--text-secondary, #667085);
  font-size: 0.88rem;
}
.error-message {
  color: #a52125;
}
.notice {
  color: #276249;
}
</style>
