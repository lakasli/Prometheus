<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>();
const contexts = ref<any[]>([]),
  transfers = ref<any[]>([]),
  run = ref(""),
  kind = ref("image"),
  caption = ref(""),
  confirmed = ref(false),
  asset = ref<any>(null),
  busy = ref(false),
  notice = ref(""),
  file = ref<File | null>(null);
const uploadRequest = ref(crypto.randomUUID()),
  sendRequest = ref(crypto.randomUUID());
let disposed = false,
  generation = 0;
watch([run, kind], () => {
  generation++;
  asset.value = null;
  confirmed.value = false;
  uploadRequest.value = crypto.randomUUID();
  sendRequest.value = crypto.randomUUID();
});
async function task(action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  notice.value = "";
  try {
    await action();
  } catch (e) {
    if (!disposed) emit("error", e);
  } finally {
    busy.value = false;
  }
}
async function load() {
  const [c, t] = await Promise.all([
    api<any>("/qq/media/contexts"),
    api<any>("/qq/media/deliveries"),
  ]);
  if (disposed) return;
  contexts.value = c.items;
  transfers.value = t.items;
  if (run.value && !c.items.some((r: any) => r.run_id === run.value))
    run.value = "";
}
function selectFile(event: Event) {
  generation++;
  asset.value = null;
  confirmed.value = false;
  uploadRequest.value = crypto.randomUUID();
  sendRequest.value = crypto.randomUUID();
  file.value = (event.target as HTMLInputElement).files?.[0] ?? null;
}
async function upload() {
  await task(async () => {
    const selected = file.value,
      scope = run.value,
      type = kind.value,
      epoch = generation;
    if (!selected || !scope) throw Error("请先选择本次私聊回复和文件。");
    if (
      !selected.size ||
      selected.size > (type === "image" ? 8 : 20) * 1024 * 1024
    )
      throw Error(
        type === "image" ? "图片须为8MiB以内。" : "文件须为20MiB以内。",
      );
    const encoded = await new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onerror = () => reject(Error("文件未能读取。"));
      reader.onload = () => resolve(String(reader.result).split(",")[1] ?? "");
      reader.readAsDataURL(selected);
    });
    const result = await api("/qq/media/assets", "POST", {
      run_id: scope,
      kind: type,
      data_base64: encoded,
      request_id: uploadRequest.value,
    });
    if (disposed || generation !== epoch) return;
    asset.value = result;
    notice.value = "文件已校验并暂存，尚未发送。";
  });
}
async function send() {
  await task(async () => {
    if (!asset.value || !confirmed.value)
      throw Error("请核对当前接收方并确认发送。");
    const result = await api<any>("/qq/media/deliveries", "POST", {
      run_id: run.value,
      asset_id: asset.value.id,
      text: caption.value,
      request_id: sendRequest.value,
      acknowledge_effects: true,
    });
    confirmed.value = false;
    asset.value = null;
    notice.value =
      result.status === "queued"
        ? "媒体已排队；上传、平台确认和客户端播放分别核对。"
        : "发送记录已保存。";
    await load();
  });
}
const stage = (value: string) =>
  (
    ({
      queued: "等待上传",
      preparing: "申请上传",
      uploading: "正在上传",
      confirming: "确认分片",
      finalizing: "取得媒体引用",
      ready: "等待发送",
      sending: "正在发送",
      sent: "平台已确认",
      failed: "未完成",
      unknown: "发送结果未知",
      cancelled: "已取消",
    }) as Record<string, string>
  )[value] ?? value;
onMounted(() => task(load));
onUnmounted(() => {
  disposed = true;
  generation++;
  file.value = null;
});
</script>
<template>
  <section class="panel form-panel" aria-label="QQ与微信媒体发送">
    <div class="section-heading">
      <div>
        <h2>QQ 与微信媒体回复</h2>
        <p>
          向 QQ
          或微信本人私聊追加图片、视频或已有音频；微信支持普通文件，收件人由原消息固定。
        </p>
      </div>
      <button class="secondary" :disabled="busy" @click="task(load)">
        刷新媒体记录
      </button>
    </div>
    <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
    <p v-if="!contexts.length" class="muted">
      当前没有可追加媒体的私聊回复。需要原回复已入队、回复期限有效且未超过四项。
    </p>
    <form @submit.prevent="upload">
      <label
        >本次私聊回复<select v-model="run" :disabled="busy">
          <option value="">选择实际收到的消息</option>
          <option v-for="c in contexts" :key="c.run_id" :value="c.run_id">
            {{ c.channel === "weixin" ? "微信" : "QQ" }} ·
            {{ new Date(c.received_at).toLocaleString() }} · {{ c.target_id }} ·
            剩余{{ c.remaining_parts }}项
          </option>
        </select></label
      >
      <p v-if="run" class="muted">
        接收方标识：{{ contexts.find((c) => c.run_id === run)?.target_id }}
      </p>
      <label
        >文件类型<select v-model="kind" :disabled="busy">
          <option value="image">图片</option>
          <option value="video">MP4 视频</option>
          <option value="voice">已有音频</option>
          <option
            v-if="contexts.find((c) => c.run_id === run)?.channel === 'weixin'"
            value="file"
          >
            普通文件（含其他音频）
          </option>
        </select></label
      >
      <label
        >选择媒体文件<input
          type="file"
          :accept="
            kind === 'file'
              ? undefined
              : 'image/png,image/jpeg,image/gif,image/webp,image/bmp,video/mp4,audio/wav,audio/mpeg,audio/ogg,audio/silk'
          "
          :disabled="busy"
          @change="selectFile"
      /></label>
      <small
        >图片最多8MiB，会规范化；视频与音频最多20MiB，MP4限60秒、自包含H.264轨道。已有音频传输不启用语音合成或识别。暂存24小时。QQ超过5MiB自动分片；微信加密上传，语音限SILK/MP3，其他音频须选择普通文件。</small
      >
      <button class="secondary" :disabled="busy || !run || !file">
        校验并暂存文件
      </button>
    </form>
    <form v-if="asset" @submit.prevent="send">
      <p class="muted">
        已暂存 {{ asset.kind }} · {{ asset.bytes }} 字节；尚未发送。
      </p>
      <label
        >发送说明（微信单独占一项文字）<input
          v-model="caption"
          maxlength="1500"
          :disabled="busy"
      /></label>
      <label class="checkbox"
        ><input
          v-model="confirmed"
          type="checkbox"
          :disabled="busy"
        />已核对接收方，确认在这次私聊回复中发送媒体</label
      >
      <button class="primary" :disabled="busy || !confirmed">
        确认排队发送
      </button>
    </form>
    <article
      v-for="entry in transfers"
      :key="entry.id"
      class="panel"
      data-testid="qq-media-transfer"
    >
      <strong
        >{{ entry.channel === "weixin" ? "微信" : "QQ" }} · 第{{
          entry.sequence
        }}项 · {{ entry.kind }}</strong
      >
      <p>{{ stage(entry.stage) }} · 已确认{{ entry.completed_parts }}个分片</p>
      <p v-if="entry.error" class="muted">{{ entry.error }}</p>
      <small
        >投递状态：{{
          stage(entry.status)
        }}；平台确认不代表客户端已播放。</small
      >
    </article>
  </section>
</template>
