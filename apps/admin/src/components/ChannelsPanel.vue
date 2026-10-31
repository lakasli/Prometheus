<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import QRCode from "qrcode";
import QqMediaPanel from "./QqMediaPanel.vue";
import { api } from "../api";
import type { ConfigState } from "../types";
const emit = defineEmits<{ error: [unknown] }>();
const state = ref<ConfigState | null>(null),
  config = ref<ConfigState["config"] | null>(null),
  status = ref<any>(null),
  inbox = ref<any[]>([]),
  secret = ref(""),
  busy = ref(false),
  notice = ref(""),
  login = ref<any>(null),
  qr = ref(""),
  code = ref("");
let disposed = false,
  timer: ReturnType<typeof setTimeout> | undefined;
async function task(action: () => Promise<void>) {
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
async function load() {
  const [current, live, received] = await Promise.all([
    api<ConfigState>("/config"),
    api("/channels/status"),
    api<any>("/channels/inbox"),
  ]);
  if (disposed) return;
  state.value = current;
  config.value = JSON.parse(JSON.stringify(current.config));
  status.value = live;
  inbox.value = received.items;
}
async function refreshInbox() {
  const received = await api<any>("/channels/inbox");
  if (!disposed) inbox.value = received.items;
}
const receiveLabel = (status: string) =>
  ({
    queued: "等待接收处理",
    processing: "正在下载与校验",
    completed: "已进入会话",
    expired: "回复期限已过",
    cancelled: "已取消",
    failed: "接收未完成",
  })[status] ?? status;
const receiveReason = (entry: any) =>
  entry.code
    ? ((
        {
          media_video_unread: "视频已接收和校验，尚未读取画面或声音。",
          media_voice_unread: "音频已接收和校验，尚未语音识别。",
          media_analysis_queued: "媒体已校验，正在等待理解。",
          audio_transcribed: "音频已完成本机转写；转写仍需核对。",
          video_frames_described: "视频采样帧已取得描述，覆盖范围见运行记录。",
          media_file_unread: "文件已接收，尚未读取或执行内容。",
          vision_provider_unavailable: "视觉服务尚未配置，这次没有读取图片。",
          media_control_command: "附件消息没有执行控制命令。",
          media_count_unsupported: "每条消息仅接收一个媒体附件。",
          media_channel_unsupported: "当前媒体仅支持QQ单聊和微信绑定本人私聊。",
          media_format_unsupported: "这类附件暂不读取。",
          media_size_limit: "附件超过接收上限。",
          legacy_media_reference_lost: "旧记录缺少原附件引用，请重新发送。",
          media_reply_failed: "图片回复未能完成，失败已如实保留。",
        } as Record<string, string>
      )[entry.code] ?? "附件下载或校验未能完成，没有可靠的识图结果。")
    : entry.error;
const runLabel = (status: string) =>
  (
    ({
      queued: "等待回复",
      running: "正在回复",
      completed: "回复已生成",
      failed: "未能生成回复",
      cancelled: "已取消",
      interrupted: "已中断",
    }) as Record<string, string>
  )[status] ?? status;
async function save() {
  await task(async () => {
    if (secret.value) {
      const saved = await api<{ id: string }>("/secrets", "POST", {
        kind: "qq_app_secret",
        value: secret.value,
      });
      secret.value = "";
      config.value!.qq.credential_ref = saved.id;
    }
    const result = await api<ConfigState>(
      "/config",
      "PUT",
      { config: config.value },
      state.value!.revision,
    );
    state.value = result;
    await load();
    notice.value = "渠道配置已保存并生效；真实收发结果请查看投递记录。";
  });
}
async function createLogin() {
  await task(async () => {
    login.value = await api("/channels/weixin/login", "POST", {
      expected_revision: status.value?.weixin.revision ?? 0,
    });
    await poll();
  });
}
async function poll() {
  if (disposed || !login.value) return;
  try {
    const current = await api<any>(`/channels/weixin/login/${login.value.id}`);
    if (disposed) return;
    login.value = current;
    if (current.display_content)
      qr.value = await QRCode.toDataURL(current.display_content, {
        width: 240,
        margin: 2,
      });
    if (
      ["confirmed", "failed", "expired", "cancelled"].includes(current.status)
    ) {
      qr.value = "";
      await load();
      if (current.status === "confirmed")
        notice.value = "微信本人身份已绑定。确认后启用文字与媒体收发。";
      return;
    }
    timer = setTimeout(() => void poll(), 1500);
  } catch (error) {
    emit("error", error);
  }
}
async function verify() {
  await task(async () => {
    login.value = await api(
      `/channels/weixin/login/${login.value.id}/verify`,
      "POST",
      { code: code.value },
      login.value.revision,
    );
    code.value = "";
  });
}
async function cancel() {
  await task(async () => {
    login.value = await api(
      `/channels/weixin/login/${login.value.id}`,
      "DELETE",
      undefined,
      login.value.revision,
    );
    qr.value = "";
    if (timer) clearTimeout(timer);
  });
}
onMounted(() => task(load));
onUnmounted(() => {
  disposed = true;
  if (timer) clearTimeout(timer);
  secret.value = "";
  code.value = "";
});
</script>
<template>
  <div class="feature-stack">
    <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
    <section v-if="config" class="panel form-panel">
      <div class="section-heading">
        <div>
          <h2>QQ 连接</h2>
          <p>官方 WebSocket 或签名 Webhook；只回应实际收到的消息。</p>
        </div>
        <span class="badge">{{ status?.connection.qq }}</span>
      </div>
      <form @submit.prevent="save">
        <label>App ID<input v-model="config.qq.app_id" maxlength="128" /></label
        ><label
          >AppSecret<input
            v-model="secret"
            type="password"
            autocomplete="off"
            placeholder="留空保留现有凭据；更新后不回显" /></label
        ><label
          >连接方式<select v-model="config.qq.transport">
            <option value="websocket">WebSocket（本机推荐）</option>
            <option value="webhook">签名 Webhook</option>
          </select></label
        ><label class="checkbox"
          ><input v-model="config.qq.enabled" type="checkbox" />启用 QQ
          文字接收与被动回复</label
        ><small
          >Webhook 固定路径 /webhooks/qq，需可信 HTTPS
          入口。普通短文本会有界合并；发送结果未知时不会自动重试。</small
        >
        <p class="muted">
          视觉模型明确开启后，QQ单聊的一张常用图片会先下载校验再识图。图片不执行控制命令；其他附件未读取时会如实说明。
        </p>
        <div class="form-actions">
          <button class="primary" :disabled="busy">保存渠道配置</button>
        </div>
      </form>
    </section>
    <section class="panel form-panel">
      <div class="section-heading">
        <div>
          <h2>微信 iLink</h2>
          <p>
            单账号扫码，只接收绑定本人的私聊文字和媒体；图片可识图，视频、音频和文件仅受控传输。
          </p>
        </div>
        <span class="badge">{{ status?.connection.weixin }}</span>
      </div>
      <p v-if="status?.weixin.configured" class="muted">
        已绑定 {{ status.weixin.account_id }} · 本人
        {{ status.weixin.linked_owner }}
      </p>
      <button
        class="secondary"
        :disabled="
          busy ||
          (login &&
            !['confirmed', 'failed', 'expired', 'cancelled'].includes(
              login.status,
            ))
        "
        @click="createLogin"
      >
        {{ status?.weixin.configured ? "重新扫码绑定" : "创建登录二维码" }}
      </button>
      <div v-if="qr" class="qr-card">
        <img :src="qr" alt="微信本人扫码登录二维码" width="240" height="240" />
        <p class="muted">
          请用本人微信扫描。二维码和验证码不会写入浏览器存储。
        </p>
      </div>
      <p v-if="login" class="muted">
        登录状态：{{
          {
            creating: "正在创建",
            wait: "等待扫码",
            scaned: "已扫描",
            need_verifycode: "需要验证码",
            confirmed: "已确认",
            expired: "已过期",
            failed: "未完成",
            cancelled: "已取消",
          }[login.status as string] || login.status
        }}
      </p>
      <form v-if="login?.status === 'need_verifycode'" @submit.prevent="verify">
        <label
          >手机验证码<input
            v-model="code"
            type="password"
            inputmode="numeric"
            maxlength="16"
            autocomplete="off" /></label
        ><button class="secondary" :disabled="busy">提交验证码</button>
      </form>
      <button
        v-if="
          login &&
          !['confirmed', 'failed', 'expired', 'cancelled'].includes(
            login.status,
          )
        "
        class="text-button"
        :disabled="busy"
        @click="cancel"
      >
        取消本次扫码</button
      ><label v-if="config" class="checkbox"
        ><input
          v-model="config.weixin.enabled"
          type="checkbox"
          :disabled="!status?.weixin.configured"
        />启用本人文字与媒体收发</label
      ><button v-if="config" class="primary" :disabled="busy" @click="save">
        保存微信状态
      </button>
    </section>
    <section class="panel">
      <div class="section-heading">
        <div>
          <h2>接收记录</h2>
          <p>
            QQ单聊支持一张常用图片、一个MP4或音频附件。只有单独启用对应能力后才识图、转写或采样视频；群聊媒体仍不读取。接收、理解、回复与实际发送分别记录。
          </p>
        </div>
        <button class="secondary" :disabled="busy" @click="task(refreshInbox)">
          刷新接收记录
        </button>
      </div>
      <p v-if="!inbox.length" class="muted">暂时没有渠道接收记录。</p>
      <div
        v-for="entry in inbox"
        :key="entry.id"
        class="panel"
        data-testid="channel-inbox-record"
      >
        <div class="section-heading">
          <strong
            >{{ entry.channel === "qq" ? "QQ" : "微信" }} ·
            {{ entry.media ? "附件" : "文字" }}</strong
          ><span class="badge">{{ receiveLabel(entry.status) }}</span>
        </div>
        <time class="muted">{{
          new Date(entry.received_at).toLocaleString()
        }}</time>
        <p v-if="entry.error || entry.code" class="muted">
          {{ receiveReason(entry) }}
        </p>
        <p v-if="entry.run_status" class="muted">
          会话处理：{{ runLabel(entry.run_status) }}；实际发送请核对投递记录。
        </p>
      </div>
    </section>
    <QqMediaPanel @error="emit('error', $event)" />
    <section class="panel">
      <h2>发送与隐私</h2>
      <p class="muted">
        输入按会话和发言者隔离。仅已确认发送的句段进入后续对话历史；未确认的发送保留为“结果未知”。本页不授予主动联系许可。
      </p>
      <button class="secondary" :disabled="busy" @click="task(load)">
        刷新连接状态
      </button>
    </section>
  </div>
</template>
