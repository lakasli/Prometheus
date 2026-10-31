<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>(),
  state = ref<any>(null),
  busy = ref(false),
  notice = ref("");
async function load() {
  try {
    state.value = await api("/media/policy");
  } catch (error) {
    emit("error", error);
  }
}
async function save() {
  busy.value = true;
  try {
    state.value = await api(
      "/media/policy",
      "PUT",
      state.value.value,
      state.value.revision,
    );
    notice.value = "媒体模型配置已保存；只启用明确确认的模型能力。";
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
    <div class="section-heading">
      <div>
        <h2>图片与生成模型</h2>
        <p>
          复用已保存的模型服务与凭据；视觉描述带来源，生成结果需通过相同的图片校验。
        </p>
      </div>
    </div>
    <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
    <label
      >视觉模型名称<input
        v-model="state.value.vision_model"
        maxlength="128" /></label
    ><label class="checkbox"
      ><input
        v-model="state.value.vision_enabled"
        type="checkbox"
      />我确认此服务支持图像输入</label
    ><label
      >生图模型名称<input
        v-model="state.value.image_model"
        maxlength="128" /></label
    ><label class="checkbox"
      ><input
        v-model="state.value.image_generation_enabled"
        type="checkbox"
      />我确认此服务支持图像生成接口</label
    >
    <p class="muted">
      图片接入支持PNG、JPEG、GIF、WebP和BMP，原图最多8MiB、单边12000像素且总计2400万像素。
      解码后保存为无元数据PNG，最多2048单边、2097152像素和2MiB；JPEG按方向信息旋转，GIF与WebP仅读首帧。
      视频、语音及渠道附件仍按各自Windows条件验收。
    </p>
    <button class="primary" :disabled="busy" @click="save">保存媒体模型</button>
  </section>
</template>
