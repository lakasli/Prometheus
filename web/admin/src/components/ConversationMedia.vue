<script setup lang="ts">
import { onUnmounted, ref, useId, watch } from "vue";
import { api, ApiError, errorMessage } from "../api";

interface Asset {
  id: string;
  kind: "image" | "video";
  mime: string;
  bytes: number;
  expires_at: number;
}
const props = defineProps<{ conversationId: string; disabled?: boolean }>();
const emit = defineEmits<{ unauthorized: [] }>();
const fieldId = useId();
const opened = ref(false);
const assets = ref<Asset[]>([]);
const busy = ref(false);
const error = ref("");
const notice = ref("");
const input = ref<HTMLInputElement | null>(null);
let controller: AbortController | null = null;
let reader: FileReader | null = null;
let epoch = 0;

function reset() {
  epoch++;
  controller?.abort();
  controller = null;
  reader?.abort();
  reader = null;
  assets.value = [];
  busy.value = false;
  error.value = notice.value = "";
  if (input.value) input.value.value = "";
}
function failed(cause: unknown) {
  if (cause instanceof ApiError && cause.status === 401) emit("unauthorized");
  else error.value = errorMessage(cause);
}
async function load() {
  if (busy.value || !opened.value) return;
  const token = epoch;
  const pending = new AbortController();
  controller = pending;
  busy.value = true;
  error.value = "";
  try {
    const result = await api<{ items: Asset[] }>(
      `/conversations/${encodeURIComponent(props.conversationId)}/media`,
      { signal: pending.signal },
    );
    if (token === epoch) assets.value = result.items;
  } catch (cause) {
    if (token === epoch && !pending.signal.aborted) failed(cause);
  } finally {
    if (token === epoch) busy.value = false;
  }
}
function toggle(event: Event) {
  opened.value = (event.target as HTMLDetailsElement).open;
  if (opened.value) void load();
}
function base64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const current = new FileReader();
    reader = current;
    current.onerror = () => reject(new Error("文件读取失败"));
    current.onabort = () => reject(new DOMException("Aborted", "AbortError"));
    current.onload = () => {
      reader = null;
      const result = current.result;
      if (typeof result !== "string" || !result.includes(","))
        reject(new Error("文件读取失败"));
      else resolve(result.slice(result.indexOf(",") + 1));
    };
    current.readAsDataURL(file);
  });
}
async function upload(event: Event) {
  if (busy.value || props.disabled) return;
  const field = event.target as HTMLInputElement;
  const file = field.files?.[0];
  if (!file) return;
  error.value = notice.value = "";
  const kind = /\.(png|jpe?g|webp|gif|bmp)$/i.test(file.name)
    ? "image"
    : file.name.toLowerCase().endsWith(".mp4")
      ? "video"
      : null;
  if (
    !kind ||
    !file.size ||
    file.size > (kind === "image" ? 8*1048576 : 20 * 1048576)
  ) {
    error.value = "请选择8MiB以内的常用格式图片，或20MiB以内、最长60秒的视频。";
    field.value = "";
    return;
  }
  const token = epoch;
  const conversation = props.conversationId;
  const pending = new AbortController();
  controller = pending;
  busy.value = true;
  try {
    const data = await base64(file);
    if (token !== epoch) return;
    const asset = await api<Asset>(
      `/conversations/${encodeURIComponent(conversation)}/media`,
      {
        method: "POST",
        body: { kind, data_base64: data },
        signal: pending.signal,
      },
    );
    if (token !== epoch) return;
    assets.value = [asset, ...assets.value];
    notice.value =
      "已保存到当前会话；在消息中明确要求回发即可，尚未发送到聊天平台。";
  } catch (cause) {
    if (token === epoch && !pending.signal.aborted) failed(cause);
  } finally {
    if (token === epoch) {
      busy.value = false;
      field.value = "";
    }
  }
}
function url(asset: Asset) {
  if (!/^[a-f0-9-]{36}$/.test(asset.id)) return undefined;
  return `/api/admin/v1/conversations/${encodeURIComponent(props.conversationId)}/media/${asset.id}`;
}
watch(
  () => props.conversationId,
  () => {
    reset();
    void load();
  },
);
onUnmounted(reset);
</script>

<template>
  <details class="conversation-media" @toggle="toggle">
    <summary>当前会话的私有媒体附件</summary>
    <p class="muted">
      附件最多保留一天，仅当前身份与会话可用。此处上传不进行图片或视频理解；图片提问请使用上方消息附件入口。视频仅支持保存和回发。
    </p>
    <div class="media-actions">
      <label :for="fieldId">添加图片 / 视频</label>
      <input
        :id="fieldId"
        ref="input"
        type="file"
        accept="image/png,image/jpeg,image/webp,image/gif,image/bmp,video/mp4,.png,.jpg,.jpeg,.webp,.gif,.bmp,.mp4"
        :disabled="busy || disabled"
        @change="upload"
      />
      <button
        class="button secondary small"
        type="button"
        :disabled="busy"
        @click="load"
      >
        刷新附件
      </button>
    </div>
    <p v-if="error" role="alert">{{ error }}</p>
    <p v-if="notice" role="status">{{ notice }}</p>
    <p v-if="busy" role="status">正在处理附件…</p>
    <p v-else-if="!assets.length" class="muted">当前没有可用的媒体附件。</p>
    <ul v-if="assets.length" class="media-list">
      <li v-for="asset in assets" :key="asset.id">
        <img
          v-if="asset.kind === 'image' && url(asset)"
          :src="url(asset)"
          alt="当前会话图片附件"
          loading="lazy"
        />
        <video
          v-else-if="asset.kind === 'video' && url(asset)"
          :src="url(asset)"
          controls
          preload="none"
        />
        <small
          >{{ asset.kind === "video" ? "视频" : "图片" }} ·
          {{ (asset.bytes / 1024).toFixed(1) }} KiB ·
          {{ new Date(asset.expires_at * 1000).toLocaleString() }} 到期</small
        >
      </li>
    </ul>
  </details>
</template>

<style scoped>
.conversation-media {
  padding: 1rem;
  border-top: 1px solid var(--line);
}
summary {
  cursor: pointer;
}
.muted,
small {
  color: var(--muted);
  font-size: 0.8rem;
}
.media-actions {
  display: flex;
  align-items: center;
  gap: 0.7rem;
  flex-wrap: wrap;
}
.media-actions input {
  max-width: 100%;
}
.media-list {
  list-style: none;
  margin: 1rem 0 0;
  padding: 0;
  display: grid;
  gap: 1rem;
}
.media-list li {
  display: grid;
  gap: 0.4rem;
}
.media-list img,
.media-list video {
  max-width: 100%;
  max-height: 240px;
  border-radius: 0.6rem;
}
</style>
