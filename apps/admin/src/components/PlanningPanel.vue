<script setup lang="ts">
import { ref } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>(),
  kind = ref("trigger_draft"),
  request = ref(""),
  hours = ref(24),
  job = ref<any>(null),
  notice = ref(""),
  busy = ref(false);
async function act(action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  notice.value = "";
  try {
    await action();
  } catch (error) {
    emit("error", error);
  } finally {
    busy.value = false;
  }
}
async function create() {
  await act(async () => {
    const start = Math.floor(Date.now() / 1000) + 60;
    job.value = await api("/knowledge/jobs", "POST", {
      kind: kind.value,
      sources: [],
      options:
        kind.value === "schedule_plan"
          ? {
              request: request.value,
              window_start: start,
              window_end: start + hours.value * 3600,
            }
          : { request: request.value },
      idempotency_key: crypto.randomUUID(),
    });
  });
}
async function refresh() {
  await act(async () => {
    job.value = await api(`/knowledge/jobs/${job.value.id}`);
  });
}
async function review(approve: boolean) {
  await act(async () => {
    await api(
      `/knowledge/jobs/${job.value.id}/review`,
      "POST",
      { approve },
      job.value.revision,
    );
    job.value = await api(`/knowledge/jobs/${job.value.id}`);
    notice.value = approve
      ? "已审核保存。触发规则仍须单独预演并启用。"
      : "已拒绝并清除候选内容。";
  });
}
</script>
<template>
  <section class="panel feature-section">
    <h2>文字规划与触发建议</h2>
    <p class="muted">
      明确提出要求后生成候选，审核后保存。文字安排不会授予发送、文件或程序执行权限。
    </p>
    <form @submit.prevent="create">
      <div class="form-row">
        <label
          >建议类型<select v-model="kind">
            <option value="trigger_draft">设置触发任务</option>
            <option value="schedule_plan">有界日程规划</option>
          </select></label
        ><label v-if="kind === 'schedule_plan'"
          >规划窗口小时数<input
            v-model.number="hours"
            type="number"
            min="1"
            max="168"
            required
        /></label>
      </div>
      <label
        >规划要求<textarea
          v-model="request"
          rows="3"
          maxlength="4000"
          required
        ></textarea></label
      ><button class="primary" :disabled="busy">生成待审核建议</button>
    </form>
    <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
    <div v-if="job" class="job-card">
      <span class="badge">{{
        job.status === "review_pending"
          ? "待审核"
          : job.status === "approved"
            ? "已审核"
            : job.status
      }}</span
      ><button class="text-button" :disabled="busy" @click="refresh">
        刷新建议
      </button>
      <p v-if="job.error" class="alert error">{{ job.error }}</p>
      <pre v-if="job.output" class="preview">{{
        JSON.stringify(job.output, null, 2)
      }}</pre>
      <div v-if="job.status === 'review_pending'" class="actions">
        <button class="primary" :disabled="busy" @click="review(true)">
          审核并保存建议</button
        ><button class="secondary" :disabled="busy" @click="review(false)">
          拒绝建议
        </button>
      </div>
    </div>
  </section>
</template>
