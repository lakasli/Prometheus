<script setup lang="ts">
import { computed } from "vue";
import type { SkillDraft } from "../types";
import { drawingHtml, drawingImage, skillCandidate } from "../skill-output";
import { taskTriggerCandidate } from "../task-output";
import type { TriggerDefinition } from "../task-triggers";
const props = withDefaults(
  defineProps<{
    content: string;
    generatedImageUrl?: string | null;
    reviewable?: boolean;
  }>(),
  { reviewable: true },
);
const emit = defineEmits<{
  review: [draft: SkillDraft];
  reviewTask: [draft: TriggerDefinition];
}>();
const image = computed(() =>
  props.generatedImageUrl &&
  /^\/api\/admin\/v1\/(?:skills\/images\/[a-f0-9-]{36}|task-triggers\/occurrences\/[a-f0-9-]{36}\/image)$/.test(
    props.generatedImageUrl,
  )
    ? props.generatedImageUrl
    : drawingImage(props.content),
);
const html = computed(() => (image.value ? null : drawingHtml(props.content)));
const candidate = computed(() => skillCandidate(props.content));
const taskCandidate = computed(() => taskTriggerCandidate(props.content));
function download() {
  const url = URL.createObjectURL(
    new Blob([html.value!], { type: "text/html;charset=utf-8" }),
  );
  const link = document.createElement("a");
  link.href = url;
  link.download = "dsh-drawing.html";
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
</script>
<template>
  <div class="skill-output">
    <template v-if="image"
      ><img
        class="skill-generated-image"
        :src="image"
        alt="生成的绘画"
        loading="lazy"
      /><a
        class="button secondary small"
        :href="image"
        download="dsh-drawing.png"
        >下载 PNG</a
      >
      <p class="muted">原生生图结果</p></template
    >
    <template v-else-if="html"
      ><iframe
        title="HTML 绘画预览"
        sandbox=""
        referrerpolicy="no-referrer"
        :srcdoc="html"
      ></iframe>
      <div class="output-actions">
        <span class="muted">HTML / SVG 代码绘制</span
        ><button class="button secondary small" @click="download">
          下载 HTML 画作
        </button>
      </div>
      <details>
        <summary>查看绘画代码</summary>
        <pre>{{ content }}</pre>
      </details></template
    >
    <div v-else class="message-content">{{ content }}</div>
    <button
      v-if="candidate && reviewable !== false"
      class="button secondary small"
      @click="emit('review', candidate)"
    >
      审核并保存为 Skill
    </button>
    <button
      v-if="taskCandidate && reviewable !== false"
      class="button secondary small"
      @click="emit('reviewTask', taskCandidate)"
    >
      审核任务草稿
    </button>
  </div>
</template>
<style scoped>
.skill-output {
  min-width: 0;
  max-width: 100%;
}
.skill-generated-image {
  display: block;
  max-width: 100%;
  max-height: 640px;
  object-fit: contain;
  border-radius: 14px;
  margin: 12px 0;
}
.skill-output iframe {
  display: block;
  width: 100%;
  height: 440px;
  border: 1px solid #dce2f3;
  border-radius: 14px;
  background: white;
}
.skill-output pre {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  font-size: 12px;
}
.output-actions {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin: 10px 0;
}
.skill-output summary {
  cursor: pointer;
  font-size: 12px;
}
.muted {
  font-size: 12px;
}
@media (max-width: 600px) {
  .skill-output iframe {
    height: 320px;
  }
  .output-actions {
    align-items: start;
    flex-direction: column;
  }
}
</style>
