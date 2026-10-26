<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, useId, watch } from "vue";
import { api, ApiError, errorMessage } from "../api";
import type { ChatSubmission, ConversationImageCapabilities } from "../types";
import type { SkillList, Skill } from "../types";
import Icon from "./Icon.vue";

const props = defineProps<{
  conversationId: string | null;
  isTest: boolean;
  modelConfigured: boolean;
  running: boolean;
  blocked: boolean;
  prepareConversation: (title: string, signal: AbortSignal) => Promise<string>;
  skillId?: string;
}>();
const emit = defineEmits<{
  accepted: [submission: ChatSubmission];
  busy: [value: boolean];
  unauthorized: [];
  inspectHistory: [];
  "update:skillId": [value: string];
}>();
const fieldId = useId();
const content = ref("");
const skills = ref<Skill[]>([]);
const skillError = ref("");
const skillChoice = computed({
  get: () => props.skillId || "",
  set: (value: string) => emit("update:skillId", value),
});
async function loadSkills() {
  const pending = request();
  try {
    const result = await api<SkillList>("/skills", {
      signal: pending.controller.signal,
    });
    if (disposed) return;
    skills.value = result.items.filter((item) => item.enabled);
    if (
      skillChoice.value &&
      !skills.value.some((item) => item.id === skillChoice.value)
    )
      skillChoice.value = "";
  } catch (caught) {
    if (disposed) return;
    if (caught instanceof ApiError && caught.status === 401) expired();
    else skillError.value = "Skill 列表读取失败，仍可发送普通消息。";
  } finally {
    pending.done();
  }
}
const capabilities = ref<ConversationImageCapabilities | null>(null);
const capabilitiesLoading = ref(true);
const imageFile = ref<File | null>(null);
const previewUrl = ref("");
const imageSize = ref<{ width: number; height: number } | null>(null);
const fileInput = ref<HTMLInputElement | null>(null);
const checking = ref(false);
const submitting = ref(false);
const uncertain = ref(false);
const discardConfirm = ref(false);
const error = ref("");
const capabilityError = ref("");
const controllers = new Set<AbortController>();
let attempt: {
  conversationId: string;
  clientId: string;
  content: string;
  imageBase64?: string;
  skillId?: string;
} | null = null;
let fileEpoch = 0;
let epoch = 0;
let disposed = false;
let preparing = false;
let cancelDecode: (() => void) | null = null;
let capabilityController: AbortController | null = null;
const hasUnconfirmed = computed(() => uncertain.value);
const locked = computed(
  () => submitting.value || props.running || props.blocked || uncertain.value,
);
const contentBytes = computed(
  () => new TextEncoder().encode(content.value).length,
);
const imageCommand = computed(
  () => !!imageFile.value && content.value.trimStart().startsWith("/"),
);
const imageAllowed = computed(
  () => !!capabilities.value?.available && !props.isTest,
);
const memoryCommand = computed(() =>
  /^\/memory(?:\s|$)/.test(content.value.trim()),
);
const canSend = computed(
  () =>
    !locked.value &&
    !checking.value &&
    !!content.value.trim() &&
    contentBytes.value <= 16000 &&
    !(imageFile.value && skillChoice.value) &&
    (imageFile.value
      ? imageAllowed.value && props.modelConfigured && !imageCommand.value
      : props.modelConfigured || memoryCommand.value),
);

function request() {
  const controller = new AbortController();
  controllers.add(controller);
  const timer = setTimeout(() => controller.abort(), 15000);
  return {
    controller,
    done: () => {
      clearTimeout(timer);
      controllers.delete(controller);
    },
  };
}
function releaseImage(clearInput = true) {
  fileEpoch++;
  cancelDecode?.();
  cancelDecode = null;
  if (previewUrl.value) URL.revokeObjectURL(previewUrl.value);
  previewUrl.value = "";
  imageFile.value = null;
  imageSize.value = null;
  checking.value = false;
  if (clearInput && fileInput.value) fileInput.value.value = "";
}
function clearDraft() {
  epoch++;
  for (const controller of controllers) controller.abort();
  controllers.clear();
  releaseImage();
  content.value = "";
  attempt = null;
  uncertain.value = false;
  discardConfirm.value = false;
  error.value = "";
}
function expired() {
  clearDraft();
  emit("unauthorized");
}
async function loadCapabilities() {
  const pending = request();
  controllers.delete(pending.controller);
  capabilityController = pending.controller;
  try {
    const result = await api<ConversationImageCapabilities>(
      "/conversation-images/capabilities",
      { signal: pending.controller.signal },
    );
    if (!disposed && !pending.controller.signal.aborted)
      capabilities.value = result;
  } catch (caught) {
    if (!disposed) {
      if (caught instanceof ApiError && caught.status === 401) expired();
      else capabilityError.value = "图片能力暂时未能确认，文字对话仍可使用。";
    }
  } finally {
    pending.done();
    if (!disposed) capabilitiesLoading.value = false;
  }
}
function decode(url: string) {
  return new Promise<{ width: number; height: number }>((resolve, reject) => {
    const image = new Image();
    const cleanup = () => {
      clearTimeout(timer);
      image.onload = null;
      image.onerror = null;
      cancelDecode = null;
    };
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error("本地图片解码超时，请重新选择 PNG。"));
    }, 10000);
    cancelDecode = () => {
      cleanup();
      reject(new DOMException("cancelled", "AbortError"));
    };
    image.onload = () => {
      const size = { width: image.naturalWidth, height: image.naturalHeight };
      cleanup();
      resolve(size);
    };
    image.onerror = () => {
      cleanup();
      reject(new Error("图片无法解码，请选择完整的 PNG 文件。"));
    };
    image.decoding = "async";
    image.src = url;
  });
}
async function chooseImage(event: Event) {
  if (locked.value || !imageAllowed.value) return;
  const selected = (event.target as HTMLInputElement).files;
  const chosen = selected?.[0];
  releaseImage(false);
  error.value = "";
  attempt = null;
  if (!chosen) return;
  const token = fileEpoch;
  checking.value = true;
  try {
    const caps = capabilities.value!;
    if (selected!.length !== 1 || (chosen.type && chosen.type !== "image/png"))
      throw new Error("每条消息只能附一张 PNG 图片。");
    if (!chosen.size || chosen.size > Math.min(1048576, caps.max_image_bytes))
      throw new Error("PNG 不能为空，且不能超过 1 MiB。");
    const header = await chosen.slice(0, 33).arrayBuffer();
    const values = new Uint8Array(header);
    if (
      values.length < 33 ||
      ![137, 80, 78, 71, 13, 10, 26, 10].every(
        (value, index) => values[index] === value,
      )
    )
      throw new Error("请选择有效 PNG 图片。");
    const data = new DataView(header);
    if (data.getUint32(8) !== 13 || data.getUint32(12) !== 0x49484452)
      throw new Error("PNG 文件头无效。");
    const width = data.getUint32(16);
    const height = data.getUint32(20);
    if (
      !width ||
      !height ||
      width > Math.min(2048, caps.max_width) ||
      height > Math.min(2048, caps.max_height) ||
      width * height > Math.min(2097152, caps.max_pixels)
    )
      throw new Error("图片宽高各不超过 2,048，总像素不超过 2,097,152。");
    if (disposed || token !== fileEpoch) return;
    const url = URL.createObjectURL(chosen);
    previewUrl.value = url;
    const actual = await decode(url);
    if (disposed || token !== fileEpoch) return;
    if (actual.width !== width || actual.height !== height)
      throw new Error("图片解码尺寸与 PNG 文件头不一致。");
    imageFile.value = chosen;
    imageSize.value = { width, height };
  } catch (caught) {
    if (!disposed && token === fileEpoch) {
      releaseImage();
      error.value =
        caught instanceof Error ? caught.message : "图片读取未完成。";
    }
  } finally {
    if (!disposed && token === fileEpoch) checking.value = false;
  }
}
function encode(buffer: ArrayBuffer) {
  const values = new Uint8Array(buffer);
  const chunks: string[] = [];
  for (let index = 0; index < values.length; index += 32768)
    chunks.push(String.fromCharCode(...values.subarray(index, index + 32768)));
  return btoa(chunks.join(""));
}
async function submit(retry = false) {
  if (
    retry
      ? !uncertain.value ||
        !attempt ||
        submitting.value ||
        props.running ||
        props.blocked
      : !canSend.value
  )
    return;
  const token = epoch;
  const pending = request();
  submitting.value = true;
  emit("busy", true);
  error.value = "";
  let messageDispatched = false;
  try {
    if (!retry) {
      const originalContent = content.value.trim();
      const originalImage = imageFile.value;
      preparing = true;
      const id = await props.prepareConversation(
        originalContent.slice(0, 28),
        pending.controller.signal,
      );
      preparing = false;
      if (disposed || token !== epoch || pending.controller.signal.aborted)
        return;
      const base64 = originalImage
        ? encode(await originalImage.arrayBuffer())
        : undefined;
      if (disposed || token !== epoch || pending.controller.signal.aborted)
        return;
      attempt = {
        conversationId: id,
        clientId: crypto.randomUUID(),
        content: originalContent,
        imageBase64: base64,
        skillId: skillChoice.value || undefined,
      };
    }
    const frozen = attempt!;
    const path = `/conversations/${encodeURIComponent(frozen.conversationId)}/${frozen.imageBase64 === undefined ? "messages" : "image-messages"}`;
    messageDispatched = true;
    const result = await api<ChatSubmission>(path, {
      method: "POST",
      body: {
        content: frozen.content,
        client_message_id: frozen.clientId,
        ...(frozen.skillId ? { skill_id: frozen.skillId } : {}),
        ...(frozen.imageBase64 === undefined
          ? {}
          : { image_base64: frozen.imageBase64 }),
      },
      signal: pending.controller.signal,
    });
    if (disposed || token !== epoch || pending.controller.signal.aborted)
      return;
    if (
      !result.run_id ||
      (result.conversation_id &&
        result.conversation_id !== frozen.conversationId)
    )
      throw new Error("invalid submission response");
    emit("accepted", {
      run_id: result.run_id,
      conversation_id: frozen.conversationId,
    });
    releaseImage();
    content.value = "";
    skillChoice.value = "";
    attempt = null;
    uncertain.value = false;
  } catch (caught) {
    if (disposed || token !== epoch) return;
    if (caught instanceof ApiError && caught.status === 401) expired();
    else if (
      caught instanceof ApiError &&
      caught.status >= 400 &&
      caught.status < 500 &&
      caught.status !== 409
    ) {
      error.value = errorMessage(caught);
      uncertain.value = false;
      attempt = null;
    } else if (messageDispatched && attempt) {
      uncertain.value = true;
      error.value =
        "消息提交结果未确认。正文、图片与请求编号已冻结；不会自动重发。请查看会话历史，或原样重试以取回同一任务。";
    } else {
      error.value = "会话未能准备，尚未重发消息。请确认连接后重试。";
      attempt = null;
    }
  } finally {
    preparing = false;
    pending.done();
    if (!disposed) {
      submitting.value = false;
      emit("busy", false);
    }
  }
}
function keydown(event: KeyboardEvent) {
  if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    if (!uncertain.value) void submit();
  }
}
watch(
  () => props.conversationId,
  (id, before) => {
    if (preparing && before === null && !!id) return;
    clearDraft();
  },
);
watch(
  () => props.isTest,
  (value) => {
    if (value && !uncertain.value) releaseImage();
  },
);
onMounted(() => void loadCapabilities());
onMounted(() => void loadSkills());
onUnmounted(() => {
  disposed = true;
  capabilityController?.abort();
  clearDraft();
  emit("busy", false);
});
defineExpose({ hasUnconfirmed });
</script>

<template>
  <form
    class="chat-composer conversation-composer"
    aria-label="发送对话消息"
    @submit.prevent="submit()"
  >
    <label class="composer-skill-choice"
      >本次 Skill
      <select
        v-model="skillChoice"
        aria-label="本次 Skill"
        :disabled="locked || !!imageFile"
      >
        <option value="">普通对话</option>
        <option v-for="skill in skills" :key="skill.id" :value="skill.id">
          {{ skill.name }} · v{{ skill.revision }}
        </option></select
      ><small>{{
        skills.find((item) => item.id === skillChoice)?.description ||
        skillError ||
        "选择后仅用于本次消息；也可输入 /skill 编号 任务内容。"
      }}</small></label
    >
    <p v-if="error" class="composer-alert" role="alert">{{ error }}</p>
    <figure v-if="previewUrl" class="conversation-preview">
      <img :src="previewUrl" alt="本次对话图片的本地预览" />
      <figcaption v-if="imageSize">
        PNG · {{ imageSize.width }} × {{ imageSize.height }} ·
        {{ uncertain ? "提交结果未确认，仅供核对" : "本地预览，尚未发送" }}
      </figcaption>
      <button
        type="button"
        class="button secondary small"
        :disabled="locked"
        @click="releaseImage()"
      >
        移除对话图片
      </button>
    </figure>
    <textarea
      v-model="content"
      rows="2"
      maxlength="16000"
      :placeholder="imageFile ? '针对这张图片问点什么…' : '给嘉卉发条消息…'"
      aria-label="对话消息"
      :disabled="locked"
      :aria-describedby="`${fieldId}-image-help`"
      @keydown="keydown"
    ></textarea>
    <p v-if="imageFile" class="conversation-image-note">
      图片由 DeepSeek Flash
      先生成视觉描述，再结合当前对话回答。独立的视觉偏好尚未接入。
    </p>
    <p v-if="imageCommand" class="composer-alert" role="alert">
      有图片时不能发送任何 / 开头的控制命令。请移除图片后再使用记忆或角色命令。
    </p>
    <p v-if="contentBytes > 16000" class="composer-alert" role="alert">
      消息不能超过 16,000 UTF-8 字节。
    </p>
    <p v-if="checking" class="conversation-image-note" role="status">
      正在本地校验图片…
    </p>
    <div v-if="uncertain" class="uncertain-message-actions">
      <button
        type="button"
        class="button secondary small"
        :disabled="submitting || running || blocked"
        @click="submit(true)"
      >
        原样重试此消息</button
      ><button
        type="button"
        class="button secondary small"
        :disabled="submitting"
        @click="emit('inspectHistory')"
      >
        查看会话历史</button
      ><button
        type="button"
        class="button secondary small"
        :disabled="submitting"
        @click="discardConfirm = true"
      >
        放弃本地草稿
      </button>
    </div>
    <section
      v-if="discardConfirm"
      class="composer-discard"
      aria-label="确认放弃未确认消息"
    >
      <p>
        只清除本地草稿与预览，不会重发或取消可能已创建的任务。可稍后查看会话历史。
      </p>
      <button type="button" class="button secondary small" @click="clearDraft">
        确认放弃本地草稿</button
      ><button
        type="button"
        class="button secondary small"
        @click="discardConfirm = false"
      >
        继续核对
      </button>
    </section>
    <div class="conversation-composer-actions">
      <label class="conversation-file-choice"
        ><span>{{ imageFile ? "更换 PNG" : "附加 PNG" }}</span
        ><input
          ref="fileInput"
          type="file"
          accept="image/png"
          aria-label="附加一张对话 PNG 图片"
          :disabled="
            locked ||
            checking ||
            capabilitiesLoading ||
            !imageAllowed ||
            !!skillChoice
          "
          @change="chooseImage" /></label
      ><span>Enter 发送 · Shift + Enter 换行</span
      ><button class="button primary small" type="submit" :disabled="!canSend">
        <span v-if="submitting || running" class="spinner"></span
        >{{ submitting || running ? "处理中" : "发送"
        }}<Icon v-if="!submitting && !running" name="arrow" :size="17" />
      </button>
    </div>
    <p :id="`${fieldId}-image-help`" class="conversation-image-help">
      {{
        isTest
          ? "草稿测试会话暂不支持图片，文字测试不受影响。"
          : capabilityError ||
            (capabilitiesLoading
              ? "正在确认图片能力…"
              : capabilities?.available
                ? "可附一张 PNG，最多 1 MiB，宽高各 2,048、总像素 2,097,152。"
                : "当前图片理解不可用，仍可发送文字。")
      }}
      本地预览不上传；原始上传文件不留存，处理中使用的规范化图片在视觉处理结束后清理。
    </p>
  </form>
</template>

<style scoped>
.composer-skill-choice {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  padding: 10px 12px;
  font-size: 12px;
  color: var(--muted);
}
.composer-skill-choice select {
  min-width: 0;
  max-width: 100%;
  padding: 6px 9px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: white;
  color: var(--ink);
}
.composer-skill-choice small {
  flex-basis: 100%;
  line-height: 1.5;
}
.conversation-composer,
.conversation-composer > * {
  min-width: 0;
}
.conversation-composer > .conversation-composer-actions {
  flex-wrap: wrap;
  gap: 9px;
}
.conversation-composer textarea {
  width: 100%;
  max-width: 100%;
  min-width: 0;
}
.conversation-file-choice {
  position: relative;
  display: inline-flex;
  align-items: center;
  border: 1px solid var(--line);
  border-radius: 7px;
  padding: 6px 9px;
  color: var(--indigo);
  font-size: 11px;
  cursor: pointer;
}
.conversation-file-choice input {
  position: absolute;
  inset: 0;
  width: 100%;
  opacity: 0;
  cursor: pointer;
  min-width: 0;
}
.conversation-file-choice:has(input:disabled) {
  opacity: 0.5;
  cursor: default;
}
.conversation-file-choice:focus-within {
  outline: 2px solid var(--indigo);
  outline-offset: 2px;
}
.conversation-preview {
  padding: 12px;
  margin: 10px 12px 0;
  border: 1px solid var(--line);
  border-radius: 8px;
}
.conversation-preview img {
  display: block;
  max-width: 100%;
  max-height: 180px;
  object-fit: contain;
}
.conversation-preview figcaption {
  color: var(--muted);
  font-size: 11px;
  line-height: 1.8;
  margin: 8px 0;
  overflow-wrap: anywhere;
}
.conversation-image-note,
.conversation-image-help,
.composer-alert,
.composer-discard {
  font-size: 11px;
  line-height: 1.8;
  margin: 8px 13px 12px;
  overflow-wrap: anywhere;
}
.conversation-image-help,
.conversation-image-note {
  color: var(--muted);
}
.composer-alert {
  color: #a23c3c;
}
.conversation-composer > .uncertain-message-actions {
  justify-content: flex-start;
  flex-wrap: wrap;
  padding: 10px 12px;
}
.composer-discard {
  padding: 12px;
  border: 1px solid var(--line);
  border-radius: 8px;
}
.composer-discard button {
  margin: 8px 8px 0 0;
}
.conversation-composer button {
  white-space: normal;
  max-width: 100%;
}
</style>
