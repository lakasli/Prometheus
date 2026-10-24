<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, useId } from "vue";
import { api, ApiError, errorMessage } from "../api";
import { formatDate } from "../validation";
import type { Timestamp } from "../types";

type Status =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "expired"
  | "deleted";
interface Job {
  id: string;
  status: Status;
  revision: number;
  created_at: Timestamp;
  expires_at: Timestamp;
  error_code?: string | null;
  question?: string | null;
  model: string;
  result?: string | null;
  width?: number | null;
  height?: number | null;
  image_sha256?: string | null;
}
interface View {
  available: boolean;
  provider: string;
  model: string;
  credential_configured: boolean;
  items: Job[];
  policy: {
    max_image_bytes: number;
    max_width: number;
    max_height: number;
    max_pixels: number;
    max_question_bytes: number;
    result_ttl_seconds: number;
  };
}

const fieldId = useId();
const view = ref<View | null>(null);
const question = ref("请描述图片中的主要内容。");
const file = ref<File | null>(null);
const fileInput = ref<HTMLInputElement | null>(null);
const previewUrl = ref("");
const dimensions = ref<{ width: number; height: number } | null>(null);
const checkingFile = ref(false);
const fileError = ref("");
const loading = ref(false);
const sending = ref(false);
const deletingId = ref("");
const confirmationId = ref("");
const conflict = ref(false);
const sessionExpired = ref(false);
const error = ref("");
const notice = ref("");
const controllers = new Set<AbortController>();
let listController: AbortController | null = null;
let pollController: AbortController | null = null;
let pollTimer: ReturnType<typeof setTimeout> | undefined;
let expiryTimer: ReturnType<typeof setTimeout> | undefined;
let cancelDecode: (() => void) | null = null;
let fileEpoch = 0;
let epoch = 0;
let listEpoch = 0;
let pollCount = 0;
let pollIndex = 0;
let submissionId = "";
let submissionQuestion = "";
let disposed = false;

const limits = computed(() => ({
  max_image_bytes: Math.min(
    1048576,
    view.value?.policy.max_image_bytes ?? 1048576,
  ),
  max_width: Math.min(2048, view.value?.policy.max_width ?? 2048),
  max_height: Math.min(2048, view.value?.policy.max_height ?? 2048),
  max_pixels: Math.min(2097152, view.value?.policy.max_pixels ?? 2097152),
  max_question_bytes: Math.min(
    2000,
    view.value?.policy.max_question_bytes ?? 2000,
  ),
}));
const questionBytes = computed(
  () => new TextEncoder().encode(question.value).length,
);
const questionError = computed(() =>
  !question.value.trim()
    ? "请填写要分析的问题。"
    : questionBytes.value > limits.value.max_question_bytes
      ? `问题不能超过 ${limits.value.max_question_bytes.toLocaleString()} UTF-8 字节。`
      : "",
);
const activeJobs = computed(() =>
  (view.value?.items ?? []).filter(
    (item) => item.status === "queued" || item.status === "running",
  ),
);
const canSend = computed(
  () =>
    !!view.value?.available &&
    !!view.value.credential_configured &&
    !!file.value &&
    !checkingFile.value &&
    !fileError.value &&
    !questionError.value &&
    !activeJobs.value.length &&
    !sending.value &&
    !loading.value &&
    !deletingId.value &&
    !conflict.value &&
    !sessionExpired.value,
);
const confirmJob = computed(
  () =>
    view.value?.items.find((item) => item.id === confirmationId.value) ?? null,
);
const statusText: Record<Status, string> = {
  queued: "等待分析",
  running: "正在分析",
  completed: "分析已完成",
  failed: "分析失败，未自动重试",
  cancelled: "已取消",
  interrupted: "已中断，未自动重试",
  expired: "已到期",
  deleted: "本任务已清除",
};

function timestamp(value: Timestamp) {
  return typeof value === "number"
    ? value < 1e12
      ? value * 1000
      : value
    : Date.parse(value);
}
function live(token: number) {
  return !disposed && !sessionExpired.value && token === epoch;
}
function request() {
  const controller = new AbortController();
  controllers.add(controller);
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, 10000);
  return {
    controller,
    timedOut: () => timedOut,
    done: () => {
      clearTimeout(timer);
      controllers.delete(controller);
    },
  };
}
function stopPolling() {
  clearTimeout(pollTimer);
  pollTimer = undefined;
  pollController?.abort();
  pollController = null;
}
function releaseFile(clearInput = true) {
  fileEpoch++;
  cancelDecode?.();
  cancelDecode = null;
  if (previewUrl.value) URL.revokeObjectURL(previewUrl.value);
  previewUrl.value = "";
  file.value = null;
  dimensions.value = null;
  checkingFile.value = false;
  fileError.value = "";
  submissionId = "";
  submissionQuestion = "";
  if (clearInput && fileInput.value) fileInput.value.value = "";
}
function clearSensitive() {
  epoch++;
  listEpoch++;
  stopPolling();
  clearTimeout(expiryTimer);
  for (const controller of controllers) controller.abort();
  controllers.clear();
  releaseFile();
  question.value = "";
  view.value = null;
  confirmationId.value = "";
}
function failure(caught: unknown, fallback: string) {
  stopPolling();
  if (caught instanceof ApiError && caught.status === 401) {
    clearSensitive();
    sessionExpired.value = true;
    loading.value = false;
    sending.value = false;
    deletingId.value = "";
    error.value =
      "登录已过期，本地图片预览、问题和分析结果已清除。请重新登录。";
  } else if (caught instanceof ApiError && caught.status === 409) {
    conflict.value = true;
    confirmationId.value = "";
    error.value =
      "任务状态或保存容量已变化，未自动重试。当前展示已保留，请刷新图片任务后重新核对。";
  } else {
    error.value = `${fallback}${errorMessage(caught)}`;
  }
}
function normalized(item: Job): Job {
  if (!["queued", "running", "completed"].includes(item.status))
    return { ...item, question: undefined, result: undefined };
  return item.status === "completed" ? item : { ...item, result: undefined };
}
function upsert(item: Job) {
  if (!view.value) return;
  const clean = normalized(item);
  const index = view.value.items.findIndex((entry) => entry.id === clean.id);
  if (index < 0) view.value.items.unshift(clean);
  else view.value.items[index] = clean;
}
function scrubExpired() {
  if (!view.value) return;
  view.value.items = view.value.items.map((item) =>
    item.status !== "deleted" &&
    item.status !== "expired" &&
    timestamp(item.expires_at) <= Date.now()
      ? { ...item, status: "expired", question: undefined, result: undefined }
      : item,
  );
  if (
    confirmJob.value?.status === "expired" ||
    confirmJob.value?.status === "deleted"
  )
    confirmationId.value = "";
}
function armExpiry() {
  clearTimeout(expiryTimer);
  scrubExpired();
  const deadlines = (view.value?.items ?? [])
    .filter((item) => item.status !== "expired" && item.status !== "deleted")
    .map((item) => timestamp(item.expires_at))
    .filter((value) => Number.isFinite(value) && value > Date.now());
  const next = Math.min(...deadlines);
  if (Number.isFinite(next))
    expiryTimer = setTimeout(
      () => {
        armExpiry();
        if (!activeJobs.value.length) stopPolling();
      },
      Math.min(2147483647, Math.max(1, next - Date.now())),
    );
}
function schedulePoll() {
  clearTimeout(pollTimer);
  if (
    disposed ||
    sessionExpired.value ||
    conflict.value ||
    !activeJobs.value.length
  )
    return;
  if (pollCount >= 60) {
    error.value =
      "本次状态查询已达到上限，轮询已暂停。请手动刷新图片任务确认结果。";
    return;
  }
  pollTimer = setTimeout(() => void poll(), 2000);
}
async function refresh() {
  if (disposed || sessionExpired.value) return;
  stopPolling();
  listController?.abort();
  const token = epoch;
  const read = ++listEpoch;
  const pending = request();
  listController = pending.controller;
  loading.value = true;
  error.value = "";
  try {
    const result = await api<View>("/image-analysis", {
      signal: pending.controller.signal,
    });
    if (!live(token) || read !== listEpoch || pending.controller.signal.aborted)
      return;
    result.items = result.items.map(normalized);
    view.value = result;
    confirmationId.value = "";
    conflict.value = false;
    pollCount = 0;
    pollIndex = 0;
    armExpiry();
    if (!sending.value && !deletingId.value) schedulePoll();
  } catch (caught) {
    if (
      live(token) &&
      read === listEpoch &&
      (!pending.controller.signal.aborted || pending.timedOut())
    )
      failure(caught, "图片任务未能更新，已保留上次展示。请手动刷新。");
  } finally {
    pending.done();
    if (live(token) && read === listEpoch) loading.value = false;
  }
}
async function poll() {
  pollTimer = undefined;
  if (
    disposed ||
    sessionExpired.value ||
    conflict.value ||
    !activeJobs.value.length
  )
    return;
  if (loading.value || sending.value || deletingId.value) {
    schedulePoll();
    return;
  }
  const target = activeJobs.value[pollIndex++ % activeJobs.value.length]!;
  const token = epoch;
  const pending = request();
  pollController = pending.controller;
  pollCount++;
  try {
    const result = await api<Job>(
      `/image-analysis/jobs/${encodeURIComponent(target.id)}`,
      { signal: pending.controller.signal },
    );
    if (!live(token) || pending.controller.signal.aborted) return;
    if (result.id !== target.id) throw new Error("mismatched image job");
    upsert(result);
    armExpiry();
    schedulePoll();
  } catch (caught) {
    if (
      live(token) &&
      (!pending.controller.signal.aborted || pending.timedOut())
    )
      failure(caught, "状态查询已停止，请手动刷新图片任务。");
  } finally {
    pending.done();
    if (pollController === pending.controller) pollController = null;
  }
}
function pngSize(header: ArrayBuffer) {
  const values = new Uint8Array(header);
  const signature = [137, 80, 78, 71, 13, 10, 26, 10];
  if (
    values.length < 33 ||
    !signature.every((value, index) => values[index] === value)
  )
    throw new Error("请选择有效的 PNG 图片。");
  const data = new DataView(header);
  if (data.getUint32(8) !== 13 || data.getUint32(12) !== 0x49484452)
    throw new Error("PNG 文件头无效。");
  const width = data.getUint32(16);
  const height = data.getUint32(20);
  if (
    !width ||
    !height ||
    width > limits.value.max_width ||
    height > limits.value.max_height
  )
    throw new Error(
      `图片宽高均需在 1–${Math.min(limits.value.max_width, limits.value.max_height)} 像素范围内。`,
    );
  if (width * height > limits.value.max_pixels)
    throw new Error(
      `图片总像素不能超过 ${limits.value.max_pixels.toLocaleString()}。`,
    );
  return { width, height };
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
      reject(new Error("图片无法在本地解码，请选择完整的 PNG 文件。"));
    };
    image.decoding = "async";
    image.src = url;
  });
}
async function chooseFile(event: Event) {
  const chosen = (event.target as HTMLInputElement).files?.[0];
  // The change event already contains the new selection. Release the older
  // preview without clearing the native control's new filename.
  releaseFile(false);
  if (!chosen || disposed || sessionExpired.value) return;
  const token = fileEpoch;
  checkingFile.value = true;
  try {
    if (chosen.type && chosen.type !== "image/png")
      throw new Error("此工作台只接受一张 PNG 图片。");
    if (!chosen.size || chosen.size > limits.value.max_image_bytes)
      throw new Error("PNG 图片不能为空，且不能超过 1 MiB。");
    const size = pngSize(await chosen.slice(0, 33).arrayBuffer());
    if (disposed || token !== fileEpoch || sessionExpired.value) return;
    const url = URL.createObjectURL(chosen);
    previewUrl.value = url;
    const decoded = await decode(url);
    if (disposed || token !== fileEpoch || sessionExpired.value) return;
    if (decoded.width !== size.width || decoded.height !== size.height)
      throw new Error("PNG 解码尺寸与文件头不一致。");
    file.value = chosen;
    dimensions.value = size;
  } catch (caught) {
    if (!disposed && token === fileEpoch) {
      const message =
        caught instanceof Error ? caught.message : "图片未能读取，请重新选择。";
      releaseFile();
      fileError.value = message;
    }
  } finally {
    if (token === fileEpoch && !disposed) checkingFile.value = false;
  }
}
function base64(buffer: ArrayBuffer) {
  const values = new Uint8Array(buffer);
  const parts: string[] = [];
  for (let offset = 0; offset < values.length; offset += 32768)
    parts.push(String.fromCharCode(...values.subarray(offset, offset + 32768)));
  return btoa(parts.join(""));
}
async function submit() {
  if (!canSend.value || !file.value) return;
  const chosen = file.value;
  const token = epoch;
  const sourceToken = fileEpoch;
  const requestedQuestion = question.value.trim();
  const pending = request();
  stopPolling();
  sending.value = true;
  error.value = "";
  notice.value = "";
  try {
    const buffer = await chosen.arrayBuffer();
    if (
      !live(token) ||
      sourceToken !== fileEpoch ||
      pending.controller.signal.aborted
    )
      return;
    // A confirmed refresh allows the user to explicitly start a changed
    // request. An unchanged retry must retain its original id and payload.
    if (!submissionId || submissionQuestion !== requestedQuestion) {
      submissionId = crypto.randomUUID();
      submissionQuestion = requestedQuestion;
    }
    const result = await api<Job>("/image-analysis/jobs", {
      method: "POST",
      body: {
        client_request_id: submissionId,
        image_base64: base64(buffer),
        question: submissionQuestion,
      },
      signal: pending.controller.signal,
    });
    if (
      !live(token) ||
      sourceToken !== fileEpoch ||
      pending.controller.signal.aborted
    )
      return;
    upsert(result);
    releaseFile();
    question.value = "";
    pollCount = 0;
    notice.value =
      "图片分析任务已提交。本地预览已移除；结果不会自动进入普通聊天或记忆。";
    armExpiry();
  } catch (caught) {
    if (live(token)) {
      if (!(caught instanceof ApiError && caught.status === 401))
        conflict.value = true;
      failure(
        caught,
        "未能确认提交结果，不会自动重发图片。请刷新任务列表核对。",
      );
    }
  } finally {
    pending.done();
    if (live(token)) {
      sending.value = false;
      if (!conflict.value) schedulePoll();
    }
  }
}
async function removeJob() {
  const target = confirmJob.value;
  if (
    !target ||
    sending.value ||
    loading.value ||
    deletingId.value ||
    conflict.value ||
    sessionExpired.value
  )
    return;
  const token = epoch;
  const pending = request();
  stopPolling();
  deletingId.value = target.id;
  error.value = "";
  notice.value = "";
  try {
    await api<unknown>(
      `/image-analysis/jobs/${encodeURIComponent(target.id)}`,
      {
        method: "DELETE",
        revision: target.revision,
        signal: pending.controller.signal,
      },
    );
    if (!live(token) || pending.controller.signal.aborted) return;
    upsert({
      ...target,
      status: "deleted",
      question: undefined,
      result: undefined,
    });
    confirmationId.value = "";
    notice.value =
      "本任务在应用中的图片与分析结果已清除。原数据库/WAL及离线备份遵循独立保留策略。";
    armExpiry();
    await refresh();
  } catch (caught) {
    if (live(token)) {
      if (!(caught instanceof ApiError && caught.status === 401))
        conflict.value = true;
      failure(caught, "未能确认清除结果，不会自动重试。请刷新图片任务。");
    }
  } finally {
    pending.done();
    if (live(token)) {
      deletingId.value = "";
      if (!conflict.value) schedulePoll();
    }
  }
}

onMounted(() => void refresh());
onUnmounted(() => {
  disposed = true;
  clearSensitive();
});
</script>

<template>
  <section class="panel image-analysis-panel" aria-label="独立图片分析">
    <div class="panel-heading image-analysis-heading">
      <div>
        <h2>图片分析</h2>
        <p>独立工作台，不自动接入普通聊天或助手记忆。</p>
      </div>
      <button
        type="button"
        class="button secondary small"
        :disabled="loading || sending || !!deletingId || sessionExpired"
        @click="refresh"
      >
        刷新图片任务
      </button>
    </div>
    <p class="image-analysis-policy">
      点击发送后，所选图片及问题会发送至 DeepSeek，以 deepseek-flash
      分析。只接收一张 PNG，最多 1 MiB；宽高各不超过 2,048 像素，总像素不超过
      2,097,152。图片任务结束即不再保留用于分析的活跃图片，结果最多保留 24
      小时。模型分析可能有误，请自行核对。
    </p>
    <div v-if="error" class="alert error" role="alert">
      <span>{{ error }}</span>
    </div>
    <div v-if="notice" class="alert success" role="status">
      <span>{{ notice }}</span>
    </div>
    <p v-if="loading && !view" class="footnote" role="status">
      正在读取图片分析服务与任务…
    </p>
    <p v-if="view" class="footnote image-provider">
      服务：{{ view.provider }} · {{ view.model }} ·
      {{ view.credential_configured ? "凭证已配置" : "尚未配置凭证" }}
    </p>
    <p
      v-if="view && (!view.available || !view.credential_configured)"
      class="footnote"
    >
      当前配置尚未提供此图片分析服务。请先配置并应用可用的 DeepSeek 服务。
    </p>
    <form class="image-analysis-form" @submit.prevent="submit">
      <label class="field"
        ><span>选择本地 PNG 图片</span
        ><input
          ref="fileInput"
          type="file"
          accept="image/png"
          aria-label="选择本地 PNG 图片"
          :aria-describedby="`${fieldId}-file-help`"
          :disabled="sending || sessionExpired"
          @change="chooseFile"
        /><small :id="`${fieldId}-file-help`"
          >本地预览不会上传。PNG 签名、尺寸及解码通过后，才能手动发送。</small
        ></label
      >
      <p v-if="checkingFile" class="footnote" role="status">
        正在本地校验图片…
      </p>
      <p v-if="fileError" class="alert error" role="alert">{{ fileError }}</p>
      <figure v-if="previewUrl" class="image-local-preview">
        <img :src="previewUrl" alt="待发送 PNG 的本地预览" />
        <figcaption v-if="file && dimensions">
          {{ dimensions.width }} × {{ dimensions.height }} 像素 ·
          {{ file.size.toLocaleString() }} 字节 · 尚未发送
        </figcaption>
        <button
          type="button"
          class="button secondary small"
          :disabled="sending"
          @click="releaseFile()"
        >
          移除本地图片
        </button>
      </figure>
      <label class="field"
        ><span>要分析的问题</span
        ><textarea
          v-model="question"
          aria-label="要分析的问题"
          :aria-describedby="`${fieldId}-question-help`"
          :disabled="sending || sessionExpired || (conflict && !!submissionId)"
          rows="3"
        ></textarea
        ><small :id="`${fieldId}-question-help`"
          >{{ questionBytes.toLocaleString() }} /
          {{ limits.max_question_bytes.toLocaleString() }} UTF-8
          字节。图片与问题都只用于这次独立分析。</small
        ></label
      >
      <p v-if="questionError && question" class="footnote" role="alert">
        {{ questionError }}
      </p>
      <p v-if="conflict && submissionId" class="footnote">
        本次未确认请求的问题已暂时锁定。先刷新任务列表核对；之后保持原问题重试会沿用原请求，修改问题后再次明确发送会建立新请求。
      </p>
      <p v-if="activeJobs.length" class="footnote">
        已有图片任务等待或运行中。请等待结果，或明确取消后再提交。
      </p>
      <button type="submit" class="button primary" :disabled="!canSend">
        {{ sending ? "正在提交图片…" : "发送至 DeepSeek 并分析" }}
      </button>
    </form>
    <p class="footnote image-retention-note">
      清除或到期只针对本任务在应用中的图片与分析结果；原数据库/WAL及离线备份采用独立保留策略，不代表所有副本已彻底删除。分析结果是待核对资料，不是系统指令。
    </p>
    <div v-if="view" class="image-jobs">
      <h3>图片任务</h3>
      <p v-if="!view.items.length" class="footnote">
        尚无图片分析任务；选择本地图片不会自动创建任务。
      </p>
      <article
        v-for="(item, index) in view.items"
        :key="item.id"
        class="image-job"
        :aria-label="`图片任务 ${index + 1}`"
      >
        <div class="image-job-heading">
          <strong>{{ statusText[item.status] || "任务状态待确认" }}</strong
          ><span class="badge neutral">{{ item.model }}</span>
        </div>
        <p class="footnote">
          创建 {{ formatDate(item.created_at) }} · 到期
          {{ formatDate(item.expires_at) }}
        </p>
        <p v-if="item.width && item.height" class="footnote">
          {{ item.width }} × {{ item.height }} 像素
        </p>
        <p v-if="item.question" class="image-job-content">
          <strong>问题：</strong>{{ item.question }}
        </p>
        <p
          v-if="item.status === 'completed' && item.result"
          class="image-job-content"
          aria-label="图片分析结果"
        >
          {{ item.result }}
        </p>
        <p v-if="item.status === 'completed' && !item.result" class="footnote">
          此任务未返回可显示的结果正文，请刷新确认。
        </p>
        <p
          v-if="item.status === 'expired' || item.status === 'deleted'"
          class="footnote"
        >
          此任务的图片与结果正文不再展示；幂等记录不会因清除而自动删除。
        </p>
        <button
          v-if="item.status !== 'expired' && item.status !== 'deleted'"
          type="button"
          class="button secondary small"
          :disabled="
            loading || sending || !!deletingId || conflict || sessionExpired
          "
          @click="confirmationId = item.id"
        >
          {{
            item.status === "queued" || item.status === "running"
              ? "取消并清除此任务"
              : "清除此任务"
          }}
        </button>
      </article>
    </div>
    <section
      v-if="confirmJob"
      class="image-delete-confirm"
      aria-label="确认清除图片任务"
    >
      <h3>
        {{
          confirmJob.status === "queued" || confirmJob.status === "running"
            ? "取消并清除这次图片分析？"
            : "清除这次图片分析？"
        }}
      </h3>
      <p>
        任务创建于 {{ formatDate(confirmJob.created_at) }}，模型
        {{
          confirmJob.model
        }}。清除本任务在应用中的图片与结果；已经发出的请求可能已被 DeepSeek
        接收，原数据库/WAL及离线备份按独立策略保留。
      </p>
      <p v-if="confirmJob.question" class="image-job-content">
        {{ confirmJob.question }}
      </p>
      <div class="image-analysis-actions">
        <button
          type="button"
          class="button primary small"
          :disabled="loading || sending || !!deletingId || conflict"
          @click="removeJob"
        >
          确认取消或清除本任务</button
        ><button
          type="button"
          class="button secondary small"
          :disabled="!!deletingId"
          @click="confirmationId = ''"
        >
          返回查看
        </button>
      </div>
    </section>
  </section>
</template>

<style scoped>
.image-analysis-panel,
.image-analysis-panel > *,
.image-analysis-form,
.image-analysis-panel .field {
  min-width: 0;
}
.image-analysis-heading {
  flex-wrap: wrap;
}
.image-analysis-heading > div {
  min-width: 0;
  flex: 1 1 220px;
}
.image-analysis-policy {
  margin-bottom: 20px;
  padding: 12px 14px;
  background: var(--soft-blue);
  border-radius: 8px;
  color: var(--muted);
  font-size: 12px;
  line-height: 1.9;
}
.image-analysis-panel input,
.image-analysis-panel textarea {
  min-width: 0;
  width: 100%;
  max-width: 100%;
}
.image-analysis-panel button {
  white-space: normal;
  max-width: 100%;
  text-align: left;
}
.image-provider {
  margin-bottom: 14px;
}
.image-local-preview {
  margin: 0 0 20px;
  padding: 12px;
  border: 1px solid var(--line);
  border-radius: 9px;
}
.image-local-preview img {
  display: block;
  max-width: 100%;
  max-height: 320px;
  object-fit: contain;
  margin-bottom: 12px;
}
.image-local-preview figcaption {
  font-size: 12px;
  line-height: 1.9;
  color: var(--muted);
  margin-bottom: 10px;
  overflow-wrap: anywhere;
}
.image-retention-note {
  margin-top: 20px;
}
.image-jobs {
  margin-top: 24px;
}
.image-jobs > h3,
.image-delete-confirm h3 {
  font-size: 14px;
  margin-bottom: 12px;
}
.image-job {
  border: 1px solid var(--line);
  border-radius: 9px;
  padding: 14px;
  margin-top: 12px;
  min-width: 0;
}
.image-job-heading,
.image-analysis-actions {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 10px;
}
.image-job-heading {
  justify-content: space-between;
  font-size: 13px;
}
.image-job .footnote {
  margin-top: 10px;
}
.image-job button {
  margin-top: 12px;
}
.image-job-content {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  font-size: 13px;
  line-height: 1.9;
  margin-top: 12px;
}
.image-delete-confirm {
  margin-top: 18px;
  padding: 16px;
  border: 1px solid #e4d2b1;
  border-radius: 9px;
  background: #fdf9f1;
  min-width: 0;
}
.image-delete-confirm > p {
  font-size: 12px;
  line-height: 1.9;
  margin: 12px 0;
}
.image-analysis-actions {
  margin-top: 14px;
}
@media (max-width: 480px) {
  .image-job,
  .image-delete-confirm {
    padding: 12px;
  }
  .image-analysis-actions {
    flex-direction: column;
    align-items: stretch;
  }
}
</style>
