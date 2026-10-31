<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown]; changed: [] }>(),
  state = ref<any>(null),
  enabled = ref(false),
  busy = ref(false),
  text = ref(""),
  notice = ref(""),
  audio = ref("");
let timer: ReturnType<typeof setTimeout> | undefined,
  disposed = false,
  generation = 0,
  viewEpoch = 0;
async function load(replace = false) {
  const epoch = ++viewEpoch;
  const value = await api<any>("/speech");
  if (disposed || epoch !== viewEpoch) return;
  if (!state.value || replace) enabled.value = value.enabled;
  state.value = value;
}
async function task(fn: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  notice.value = "";
  try {
    await fn();
  } catch (e) {
    if (!disposed) emit("error", e);
  } finally {
    busy.value = false;
  }
}
async function save() {
  await task(async () => {
    const desired = enabled.value,
      c = await api<any>("/config");
    c.config.speech.enabled = desired;
    await api("/config", "PUT", { config: c.config }, c.revision);
    await load(true);
    notice.value = "语音设置已保存并生效。";
    emit("changed");
  });
}
async function poll() {
  if (disposed) return;
  if (timer) clearTimeout(timer);
  try {
    await load();
    if (
      state.value?.jobs.some((j: any) =>
        ["queued", "running", "cancelling"].includes(j.status),
      )
    )
      timer = setTimeout(() => void poll(), 1200);
  } catch (e) {
    if (!disposed) emit("error", e);
  }
}
async function synthesize() {
  await task(async () => {
    await api("/speech/syntheses", "POST", {
      text: text.value,
      request_id: crypto.randomUUID(),
    });
    text.value = "";
    notice.value = "试听已排队，只在本页提供私有音频，不发送到聊天渠道。";
    await poll();
  });
}
async function cancel(job: any) {
  await task(async () => {
    await api("/speech/jobs/" + job.id, "DELETE", undefined, job.revision);
    await poll();
  });
}
async function listen(id: string) {
  await task(async () => {
    const current = ++generation,
      response = await fetch("/api/admin/v1/speech/assets/" + id, {
        credentials: "same-origin",
      });
    if (!response.ok) throw Error("这段试听已失效，请重新生成。");
    const blob = await response.blob();
    if (disposed || current !== generation) return;
    if (audio.value) URL.revokeObjectURL(audio.value);
    audio.value = URL.createObjectURL(blob);
  });
}
async function remove(id: string) {
  await task(async () => {
    generation++;
    if (audio.value) {
      URL.revokeObjectURL(audio.value);
      audio.value = "";
    }
    await api("/speech/assets/" + id, "DELETE", {});
    notice.value = "试听音频已清理。";
    await load();
  });
}
const label = (s: string) =>
  (
    ({
      queued: "排队中",
      running: "正在合成",
      cancelling: "正在取消",
      cancelled: "已取消",
      interrupted: "服务退出，未重试",
      completed: "已生成",
      failed: "未完成",
    }) as Record<string, string>
  )[s] ?? s;
onMounted(() => task(load));
onUnmounted(() => {
  disposed = true;
  generation++;
  if (timer) clearTimeout(timer);
  if (audio.value) URL.revokeObjectURL(audio.value);
});
</script>
<template>
  <section
    v-if="state"
    class="panel feature-section form-panel"
    aria-label="本机语音"
  >
    <div class="section-heading">
      <div>
        <h2>本机中文语音</h2>
        <p>按句段生成语音；生成失败时保留文字。试听与聊天渠道发送分别确认。</p>
      </div>
      <button class="secondary" :disabled="busy" @click="task(load)">
        刷新语音状态
      </button>
    </div>
    <p class="muted">
      {{
        state.worker.installed
          ? state.worker.verified
            ? "本机运行资源已核验。"
            : "本机运行资源已安装，启用时核验可用性。"
          : "尚未安装本机语音运行资源。"
      }}
    </p>
    <label class="checkbox"
      ><input
        v-model="enabled"
        type="checkbox"
        :disabled="busy || !state.worker.installed"
      />启用本机中文语音回复</label
    >
    <small
      >固定Piper中文声音，每句最多300字，音频最多60秒；需要已有渠道回复授权。原文在本机合成，短期试听一小时后失效。声音数据许可尚未明确，本版本用于个人本机评估。</small
    >
    <button
      class="primary"
      :disabled="busy || !state.worker.installed"
      @click="save"
    >
      保存语音设置
    </button>
    <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
    <form @submit.prevent="synthesize">
      <label
        >试听文字<textarea
          v-model="text"
          maxlength="300"
          rows="3"
          :disabled="busy || !state.enabled"
        /></label
      ><button
        class="secondary"
        :disabled="busy || !state.enabled || !text.trim()"
      >
        生成私有试听
      </button>
    </form>
    <audio v-if="audio" :src="audio" controls aria-label="本次私有试听" />
    <article
      v-for="job in state.jobs"
      :key="job.id"
      class="panel"
      data-testid="speech-job"
    >
      <strong>{{ label(job.status) }}</strong
      ><small>{{ new Date(job.created_at * 1000).toLocaleString() }}</small>
      <p v-if="job.error_code" class="muted">
        这次没有生成可用音频，可检查设置后重新申请。
      </p>
      <button
        v-if="['queued', 'running'].includes(job.status)"
        class="secondary"
        :disabled="busy"
        @click="cancel(job)"
      >
        取消本次试听</button
      ><template v-if="job.result?.asset_id && job.asset_available"
        ><button
          class="secondary"
          :disabled="busy"
          @click="listen(job.result.asset_id)"
        >
          读取私有试听</button
        ><button
          class="text-button"
          :disabled="busy"
          @click="remove(job.result.asset_id)"
        >
          清理试听音频
        </button></template
      >
    </article>
  </section>
</template>
<style scoped>
audio {
  width: 100%;
  max-width: 100%;
}
</style>
