<script setup lang="ts">
import { ref } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>(),
  prompt = ref(""),
  busy = ref(false),
  job = ref<any>(null);
async function create() {
  busy.value = true;
  try {
    job.value = await api("/knowledge/jobs", "POST", {
      kind: "drawing_html",
      sources: [],
      options: { request: prompt.value },
      idempotency_key: crypto.randomUUID(),
    });
  } catch (error) {
    emit("error", error);
  } finally {
    busy.value = false;
  }
}
async function refresh() {
  if (!job.value) return;
  try {
    job.value = await api(`/knowledge/jobs/${job.value.id}`);
  } catch (error) {
    emit("error", error);
  }
}
</script>
<template>
  <section class="panel feature-section">
    <h2>绘画方法</h2>
    <p class="muted">
      创建隔离预览的HTML / SVG插画。内容不会授予脚本、文件、网络或发送权限。
    </p>
    <form @submit.prevent="create">
      <label
        >画面要求<textarea
          v-model="prompt"
          required
          maxlength="4000"
          rows="3"
        ></textarea></label
      ><button class="primary" :disabled="busy">生成隔离插画</button>
    </form>
    <div v-if="job" class="job-card">
      <span class="badge">{{
        job.status === "review_pending"
          ? "预览已就绪"
          : job.status === "failed"
            ? "生成失败"
            : "等待生成结果"
      }}</span
      ><button class="text-button" @click="refresh">刷新插画</button>
      <p v-if="job.error" class="alert error">{{ job.error }}</p>
      <iframe
        v-if="job.output?.html"
        :src="`/api/admin/v1/knowledge/jobs/${job.id}/preview`"
        sandbox=""
        title="隔离插画预览"
        class="art-preview"
      ></iframe>
    </div>
  </section>
</template>
