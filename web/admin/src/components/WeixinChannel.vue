<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, useId } from "vue";
import QRCode from "qrcode";
import { api, ApiError, errorMessage } from "../api";
import { formatDate } from "../validation";
import type { Timestamp } from "../types";

interface Account {
  account_id: string;
  enabled: boolean;
  credential_configured: boolean;
  connection_status: string;
  last_error_code: string | null;
  last_event_at: Timestamp | null;
}
interface Login {
  id: string;
  status: string;
  revision: number;
  expires_at: Timestamp;
  error_code?: string | null;
}
interface LoginResponse extends Login {
  qr_content?: string | null;
}
interface Check {
  id: string;
  status: "pending" | "passed" | "failed";
  requested_at?: Timestamp;
  completed_at?: Timestamp | null;
  error_code?: string | null;
}
interface ChannelState {
  revision: number;
  account: Account | null;
  pending_login: Login | null;
  check: Check | null;
  delivery?: {
    queued: number;
    sending: number;
    accepted: number;
    failed: number;
    unknown: number;
  };
  capabilities: {
    text_private: boolean;
    linked_owner_only: boolean;
    groups: boolean;
    media: boolean;
    max_accounts: number;
  };
}

const fieldId = useId();
const state = ref<ChannelState | null>(null);
const login = ref<Login | null>(null);
const qrUrl = ref("");
const verification = ref("");
const check = ref<Check | null>(null);
const requestedCheckId = ref("");
const busy = ref("");
const error = ref("");
const notice = ref("");
const statusError = ref("");
const stale = ref(true);
const conflict = ref(false);
const confirmDisable = ref(false);
let disposed = false;
let stateEpoch = 0;
let loginEpoch = 0;
let qrEpoch = 0;
let loginDeadline = 0;
let statusTimer: ReturnType<typeof setTimeout> | undefined;
let loginTimer: ReturnType<typeof setTimeout> | undefined;
let expiryTimer: ReturnType<typeof setTimeout> | undefined;
let statusController: AbortController | null = null;
let loginController: AbortController | null = null;
let mutationController: AbortController | null = null;
let statusRequest: Promise<void> | null = null;
let loginRequest: Promise<void> | null = null;
const activeStatuses = new Set([
  "creating",
  "waiting",
  "scanned",
  "need_verification",
  "verifying",
  "redirecting",
]);
const qrStatuses = new Set(["creating", "waiting", "scanned", "redirecting"]);
const activeLogin = computed(
  () => !!login.value && activeStatuses.has(login.value.status),
);
const connected = computed(
  () =>
    !stale.value &&
    !!state.value?.account?.enabled &&
    state.value.account.credential_configured &&
    state.value.account.connection_status === "connected",
);
const needsLogin = computed(() =>
  ["needs_login", "token_stale", "reauth_required"].includes(
    state.value?.account?.connection_status ?? "",
  ),
);
const connectionLabel = computed(() => {
  if (stale.value) return state.value ? "状态刷新未完成" : "正在读取账号状态";
  const account = state.value?.account;
  if (!account) return "尚未绑定微信";
  if (needsLogin.value)
    return "凭据失效，需要重新扫码";
  if (!account.enabled) return "已停用";
  if (connected.value) return "私聊接收已连接";
  switch (account.connection_status) {
    case "needs_login":
    case "token_stale":
    case "reauth_required":
      return "凭据失效，需要重新扫码";
    case "connecting":
      return "正在连接私聊接收";
    case "backoff":
      return "连接暂缓，稍后自动重试";
    case "error":
    case "disconnected":
      return "尚未连接，请检查状态";
    default:
      return "等待实际连接状态";
  }
});
const loginLabel = computed(() => {
  switch (login.value?.status) {
    case "creating":
      return "正在申请二维码";
    case "waiting":
      return "等待微信扫码授权";
    case "scanned":
      return "已扫码，请在微信中确认";
    case "need_verification":
      return "请填写手机微信显示的数字";
    case "verifying":
      return "正在核对配对数字";
    case "verification_blocked":
      return "配对数字验证已受限，请稍后手动重新扫码";
    case "redirecting":
      return "正在切换官方登录节点";
    case "confirmed":
      return "扫码凭据已保存，连接状态仍需核验";
    case "existing_credentials":
      return "检测到已有凭据，等待连接核验";
    case "expired":
      return "登录任务已过期，请手动重新扫码";
    case "cancelled":
      return "登录任务已取消";
    case "interrupted":
      return "登录任务已中断，请手动重新开始";
    case "failed":
      return "登录未完成，请检查错误代码";
    default:
      return "尚未开始扫码";
  }
});
const checkLabel = computed(() => {
  if (!check.value) return "尚未发起连接检查";
  const prefix = check.value.id === requestedCheckId.value ? "本次" : "上次";
  if (check.value.status === "pending") return `${prefix}检查正在进行`;
  if (check.value.status === "passed") return `${prefix}连接检查通过`;
  return `${prefix}连接检查未通过`;
});

function publicLogin(value: Login): Login {
  return {
    id: value.id,
    status: value.status,
    revision: value.revision,
    expires_at: value.expires_at,
    error_code: value.error_code ?? null,
  };
}
function publicState(value: ChannelState): ChannelState {
  const account = value.account;
  const probe = value.check;
  return {
    revision: value.revision,
    account: account
      ? {
          account_id: account.account_id,
          enabled: account.enabled,
          credential_configured: account.credential_configured,
          connection_status: account.connection_status,
          last_error_code: account.last_error_code,
          last_event_at: account.last_event_at,
        }
      : null,
    pending_login: value.pending_login
      ? publicLogin(value.pending_login)
      : null,
    check: probe
      ? {
          id: probe.id,
          status: probe.status,
          requested_at: probe.requested_at,
          completed_at: probe.completed_at,
          error_code: probe.error_code,
        }
      : null,
    delivery: value.delivery
      ? {
          queued: value.delivery.queued,
          sending: value.delivery.sending,
          accepted: value.delivery.accepted,
          failed: value.delivery.failed,
          unknown: value.delivery.unknown,
        }
      : undefined,
    capabilities: {
      text_private: value.capabilities.text_private,
      linked_owner_only: value.capabilities.linked_owner_only,
      groups: value.capabilities.groups,
      media: value.capabilities.media,
      max_accounts: value.capabilities.max_accounts,
    },
  };
}
function milliseconds(value: Timestamp) {
  return typeof value === "number"
    ? value < 1e12
      ? value * 1000
      : value
    : new Date(value).getTime();
}
function clearQr() {
  qrEpoch++;
  qrUrl.value = "";
}
function clearPrivate() {
  clearQr();
  verification.value = "";
}
function expireLogin() {
  if (!login.value || !activeStatuses.has(login.value.status)) return;
  loginEpoch++;
  loginController?.abort();
  if (loginTimer !== undefined) clearTimeout(loginTimer);
  loginTimer = undefined;
  clearPrivate();
  login.value = { ...login.value, status: "expired" };
}
function adoptLogin(value: Login, qrContent?: string | null) {
  const changed = login.value?.id !== value.id;
  const previousStatus = login.value?.status;
  if (!changed && login.value && value.revision < login.value.revision) return;
  if (changed) {
    loginEpoch++;
    loginController?.abort();
    clearPrivate();
    loginDeadline = Date.now() + 5 * 60_000;
  }
  login.value = publicLogin(value);
  if (expiryTimer !== undefined) clearTimeout(expiryTimer);
  expiryTimer = undefined;
  if (!activeStatuses.has(value.status)) {
    if (loginTimer !== undefined) clearTimeout(loginTimer);
    loginTimer = undefined;
    clearPrivate();
    return;
  }
  loginDeadline = Math.min(loginDeadline, milliseconds(value.expires_at));
  if (!Number.isFinite(loginDeadline) || loginDeadline <= Date.now()) {
    expireLogin();
    return;
  }
  expiryTimer = setTimeout(expireLogin, loginDeadline - Date.now());
  if (!qrStatuses.has(value.status)) {
    clearQr();
    if (
      value.status !== "need_verification" ||
      previousStatus !== "need_verification"
    )
      verification.value = "";
  } else if (qrContent) void renderQr(qrContent, value.id, value.revision);
  else if (qrContent === null) clearQr();
  scheduleLogin();
}
async function renderQr(content: string, id: string, revision: number) {
  const epoch = ++qrEpoch;
  qrUrl.value = "";
  if (new TextEncoder().encode(content).length > 2048) {
    error.value = "二维码内容超出本地编码上限，请取消后重试。";
    return;
  }
  try {
    const encoded = await QRCode.toDataURL(content, {
      errorCorrectionLevel: "M",
      margin: 2,
      width: 240,
    });
    if (
      disposed ||
      epoch !== qrEpoch ||
      login.value?.id !== id ||
      login.value.revision !== revision ||
      !qrStatuses.has(login.value.status) ||
      loginDeadline <= Date.now()
    )
      return;
    if (!/^data:image\/png;base64,[A-Za-z0-9+/=]+$/.test(encoded)) {
      error.value = "二维码本地编码未完成，请取消后重试。";
      return;
    }
    qrUrl.value = encoded;
  } catch {
    if (!disposed && epoch === qrEpoch)
      error.value = "二维码无法在本地编码，请取消后重试。";
  }
}
function scheduleStatus() {
  if (disposed || busy.value || statusTimer !== undefined) return;
  statusTimer = setTimeout(() => {
    statusTimer = undefined;
    void refreshStatus();
  }, 5000);
}
function scheduleLogin() {
  if (
    disposed ||
    !activeLogin.value ||
    busy.value ||
    conflict.value ||
    loginTimer !== undefined
  )
    return;
  loginTimer = setTimeout(() => {
    loginTimer = undefined;
    void refreshLogin();
  }, 2000);
}
function refreshStatus(manual = false): Promise<void> {
  if (disposed || busy.value) return Promise.resolve();
  if (statusRequest) return statusRequest;
  const epoch = stateEpoch;
  const controller = new AbortController();
  statusController = controller;
  const deadline = setTimeout(() => controller.abort(), 10_000);
  statusRequest = (async () => {
    try {
      const result = publicState(
        await api<ChannelState>("/weixin", { signal: controller.signal }),
      );
      if (disposed || controller.signal.aborted || epoch !== stateEpoch) return;
      if (state.value?.account?.account_id !== result.account?.account_id) {
        check.value = null;
        requestedCheckId.value = "";
      }
      state.value = result;
      stale.value = false;
      statusError.value = "";
      if (manual) {
        conflict.value = false;
        error.value = "";
      }
      if (
        result.check &&
        (!requestedCheckId.value || result.check.id === requestedCheckId.value)
      )
        check.value = result.check;
      if (
        result.pending_login &&
        (!login.value ||
          result.pending_login.id !== login.value.id ||
          result.pending_login.revision > login.value.revision)
      ) {
        adoptLogin(result.pending_login);
        void refreshLogin();
      }
      if (manual) {
        scheduleLogin();
        if (activeLogin.value) void refreshLogin();
      }
    } catch (caught) {
      if (!disposed && epoch === stateEpoch) {
        stale.value = true;
        statusError.value = `微信状态刷新未完成。${errorMessage(caught)}`;
        if (caught instanceof ApiError && [401, 403].includes(caught.status)) {
          clearPrivate();
          conflict.value = true;
          loginEpoch++;
          loginController?.abort();
        }
      }
    } finally {
      clearTimeout(deadline);
      statusController = null;
      statusRequest = null;
      scheduleStatus();
    }
  })();
  return statusRequest;
}
function refreshLogin(): Promise<void> {
  if (
    disposed ||
    !login.value ||
    !activeLogin.value ||
    busy.value ||
    conflict.value
  )
    return Promise.resolve();
  if (loginRequest) return loginRequest;
  const id = login.value.id;
  const epoch = loginEpoch;
  const controller = new AbortController();
  loginController = controller;
  const deadline = setTimeout(() => controller.abort(), 10_000);
  loginRequest = (async () => {
    try {
      const result = await api<LoginResponse>(
        `/weixin/login/${encodeURIComponent(id)}`,
        { signal: controller.signal },
      );
      if (
        disposed ||
        controller.signal.aborted ||
        epoch !== loginEpoch ||
        login.value?.id !== id
      )
        return;
      if (result.id !== id) {
        error.value = "登录响应与当前任务不一致，请刷新状态。";
        return;
      }
      adoptLogin(result, result.qr_content ?? null);
      if (!activeLogin.value) void refreshStatus();
    } catch (caught) {
      if (!disposed && epoch === loginEpoch && login.value?.id === id) {
        error.value = `扫码状态读取未完成。${errorMessage(caught)}`;
        if (caught instanceof ApiError && [401, 403].includes(caught.status)) {
          clearPrivate();
          conflict.value = true;
        }
      }
    } finally {
      clearTimeout(deadline);
      loginController = null;
      loginRequest = null;
      scheduleLogin();
    }
  })();
  return loginRequest;
}
async function mutate(
  label: string,
  action: (signal: AbortSignal) => Promise<void>,
) {
  if (busy.value || conflict.value || !state.value) return;
  busy.value = label;
  error.value = "";
  notice.value = "";
  stateEpoch++;
  loginEpoch++;
  statusController?.abort();
  loginController?.abort();
  if (statusTimer !== undefined) clearTimeout(statusTimer);
  if (loginTimer !== undefined) clearTimeout(loginTimer);
  statusTimer = undefined;
  loginTimer = undefined;
  await Promise.allSettled([statusRequest, loginRequest]);
  const controller = new AbortController();
  mutationController = controller;
  const deadline = setTimeout(() => controller.abort(), 15_000);
  try {
    if (!disposed) await action(controller.signal);
  } catch (caught) {
    if (!disposed) {
      if (caught instanceof ApiError && caught.status === 409) {
        conflict.value = true;
        clearPrivate();
        error.value =
          "微信配置或登录任务已在别处更新。当前状态已保留，请先刷新微信状态，再核对操作。";
      } else error.value = errorMessage(caught);
    }
  } finally {
    clearTimeout(deadline);
    mutationController = null;
    busy.value = "";
    if (!disposed) {
      await refreshStatus();
      scheduleLogin();
    }
  }
}
async function startLogin() {
  if (activeLogin.value) return;
  clearPrivate();
  await mutate("申请扫码", async (signal) => {
    const result = await api<Login>("/weixin/login", {
      method: "POST",
      body: {},
      revision: state.value!.revision,
      signal,
    });
    if (disposed) return;
    adoptLogin(result);
    notice.value = "登录任务已创建，请在五分钟内使用微信扫码并确认。";
  });
  if (!disposed && !conflict.value) void refreshLogin();
}
async function submitVerification() {
  const current = login.value;
  const code = verification.value;
  verification.value = "";
  if (!current || current.status !== "need_verification") return;
  if (!/^\d{1,16}$/.test(code)) {
    error.value = "请填写手机显示的 1 到 16 位数字。";
    return;
  }
  clearPrivate();
  await mutate("核对配对数字", async (signal) => {
    const result = await api<Login>(
      `/weixin/login/${encodeURIComponent(current.id)}/verification`,
      { method: "POST", body: { code }, revision: current.revision, signal },
    );
    if (!disposed && login.value?.id === current.id) adoptLogin(result);
  });
}
async function cancelLogin() {
  const current = login.value;
  if (!current || !activeLogin.value) return;
  clearPrivate();
  await mutate("取消登录", async (signal) => {
    const result = await api<Login>(
      `/weixin/login/${encodeURIComponent(current.id)}`,
      { method: "DELETE", revision: current.revision, signal },
    );
    if (!disposed && login.value?.id === current.id) adoptLogin(result);
  });
}
async function setEnabled(enabled: boolean) {
  if (!state.value?.account || activeLogin.value) return;
  await mutate(enabled ? "启用微信" : "停用微信", async (signal) => {
    await api("/weixin/enabled", {
      method: "POST",
      body: { enabled },
      revision: state.value!.revision,
      signal,
    });
    confirmDisable.value = false;
    notice.value = enabled
      ? "启用请求已保存，连接状态以实际接收进程为准。"
      : "微信已停用；数据保留，正在发送的结果可能仍需核对。";
  });
}
async function requestCheck() {
  if (
    !state.value?.account ||
    !state.value.account.enabled ||
    activeLogin.value
  )
    return;
  await mutate("发起连接检查", async (signal) => {
    const result = await api<{ id: string; status: "pending" }>(
      "/weixin/check",
      { method: "POST", body: {}, revision: state.value!.revision, signal },
    );
    if (disposed) return;
    requestedCheckId.value = result.id;
    check.value = { id: result.id, status: "pending" };
    notice.value = "本次连接检查已提交，等待对应检查结果。";
  });
}

onMounted(() => void refreshStatus());
onUnmounted(() => {
  disposed = true;
  stateEpoch++;
  loginEpoch++;
  statusController?.abort();
  loginController?.abort();
  mutationController?.abort();
  if (statusTimer !== undefined) clearTimeout(statusTimer);
  if (loginTimer !== undefined) clearTimeout(loginTimer);
  if (expiryTimer !== undefined) clearTimeout(expiryTimer);
  clearPrivate();
  state.value = null;
  login.value = null;
  check.value = null;
});
</script>

<template>
  <div class="weixin-workspace">
    <section class="panel" aria-label="微信私聊连接">
      <div class="panel-heading weixin-heading">
        <div>
          <h2>微信连接</h2>
          <p>
            仅接入扫码授权者的文本私聊。首期最多一个账号；群聊与媒体尚未支持。
          </p>
        </div>
        <button
          type="button"
          class="button secondary small"
          :disabled="!!busy"
          @click="refreshStatus(true)"
        >
          刷新微信状态
        </button>
      </div>
      <div v-if="error" role="alert" class="alert error">
        <span>{{ error }}</span>
      </div>
      <div v-if="statusError" role="alert" class="alert error">
        <span>{{ statusError }}</span>
      </div>
      <div v-if="notice" role="status" class="alert success">
        <span>{{ notice }}</span>
      </div>
      <dl class="detail-list">
        <div>
          <dt>实际接收状态</dt>
          <dd>
            <span
              class="badge"
              :class="connected ? 'green' : 'neutral'"
              data-testid="weixin-connection-status"
              >{{ connectionLabel }}</span
            >
          </dd>
        </div>
        <div>
          <dt>绑定账号</dt>
          <dd>{{ state?.account?.account_id || "尚未绑定" }}</dd>
        </div>
        <div>
          <dt>凭据状态</dt>
          <dd>
            {{
              state?.account?.credential_configured
                ? "已配置，仅保存在服务端"
                : "尚未保存扫码凭据"
            }}
          </dd>
        </div>
        <div>
          <dt>最近接收</dt>
          <dd>{{ formatDate(state?.account?.last_event_at) }}</dd>
        </div>
        <div v-if="state?.account?.last_error_code">
          <dt>错误代码</dt>
          <dd>
            <code>{{ state.account.last_error_code }}</code>
          </dd>
        </div>
        <div v-if="state?.delivery && !stale">
          <dt>回复投递记录</dt>
          <dd>
            待发 {{ state.delivery.queued }} · 发送中 {{ state.delivery.sending }} ·
            平台接受 {{ state.delivery.accepted }} · 失败 {{ state.delivery.failed }} ·
            结果未知 {{ state.delivery.unknown }}
          </dd>
        </div>
      </dl>
      <p
        v-if="state?.delivery && !stale && state.delivery.unknown > 0"
        class="footnote"
      >
        记录中有结果未知的发送，不会自动重试；请在手机核对，避免重复投递。平台接受也不代表手机已显示。
      </p>
      <div class="weixin-actions">
        <button
          type="button"
          class="button primary"
          :disabled="!state || !!busy || stale || conflict || activeLogin"
          @click="startLogin"
        >
          {{ state?.account ? "重新扫码授权" : "开始微信扫码" }}</button
        ><template v-if="state?.account"
          ><button
            v-if="!state.account.enabled"
            type="button"
            class="button secondary"
            :disabled="!!busy || stale || conflict || activeLogin || needsLogin"
            @click="setEnabled(true)"
          >
            启用微信</button
          ><button
            v-else
            type="button"
            class="button secondary"
            :disabled="!!busy || stale || conflict || activeLogin"
            @click="confirmDisable = true"
          >
            停用微信</button
          ><button
            type="button"
            class="button secondary"
            :disabled="
              !!busy ||
              stale ||
              conflict ||
              activeLogin ||
              !state.account.enabled ||
              check?.status === 'pending'
            "
            @click="requestCheck"
          >
            检查微信连接
          </button></template
        >
      </div>
      <section
        v-if="confirmDisable"
        class="weixin-confirm"
        aria-label="确认停用微信"
      >
        <h3>停用当前微信连接？</h3>
        <p>
          停用会取消旧的待处理任务及未发送回复；正在发送的结果可能未知，需要另行核对。已有消息和数据不会删除。
        </p>
        <div class="weixin-actions">
          <button
            type="button"
            class="button primary small"
            :disabled="!!busy || conflict"
            @click="setEnabled(false)"
          >
            确认停用微信</button
          ><button
            type="button"
            class="button secondary small"
            :disabled="!!busy"
            @click="confirmDisable = false"
          >
            取消停用
          </button>
        </div>
      </section>
      <div class="weixin-check" aria-label="微信连接检查">
        <strong>{{ checkLabel }}</strong
        ><span v-if="check?.completed_at">{{
          formatDate(check.completed_at)
        }}</span
        ><code v-if="check?.error_code">{{ check.error_code }}</code>
        <p class="footnote">
          本次检查以对应任务的结果为准；已有连接状态不代表新检查已经通过。平台接收成功也不等于手机已看到消息。
        </p>
      </div>
    </section>
    <section class="panel" aria-label="微信扫码授权">
      <div class="panel-heading">
        <div>
          <h2>扫码授权</h2>
          <p>
            二维码只在本地编码。请使用你的微信扫码，并按手机提示确认；不会自动反复刷新过期二维码。
          </p>
        </div>
      </div>
      <p class="weixin-login-state" data-testid="weixin-login-status">
        {{ loginLabel }}
      </p>
      <p v-if="activeLogin" class="footnote">
        本任务到期时间：{{
          formatDate(login?.expires_at)
        }}。最多持续五分钟；当前任务结束后才能重新开始。
      </p>
      <div v-if="qrUrl" class="weixin-qr">
        <img :src="qrUrl" alt="微信扫码授权二维码" width="240" height="240" />
        <p>仅用于当前登录任务，请勿转发。</p>
      </div>
      <form
        v-if="login?.status === 'need_verification'"
        aria-label="核对微信配对数字"
        class="weixin-verification"
        @submit.prevent="submitVerification"
      >
        <label class="field"
          ><span>微信配对数字</span
          ><input
            v-model="verification"
            type="password"
            inputmode="numeric"
            autocomplete="off"
            maxlength="16"
            aria-label="微信配对数字"
            :aria-describedby="`${fieldId}-verification-help`"
            :disabled="!!busy || conflict"
          /><small :id="`${fieldId}-verification-help`"
            >填写手机微信显示的 1 到 16
            位数字；提交或离开页面后立即清空，不保存到浏览器。</small
          ></label
        ><button
          type="submit"
          class="button primary"
          :disabled="!!busy || conflict || !verification"
        >
          提交配对数字
        </button>
      </form>
      <p v-if="login?.error_code" class="footnote">
        登录错误代码：<code>{{ login.error_code }}</code>
      </p>
      <button
        v-if="activeLogin"
        type="button"
        class="button secondary small"
        :disabled="!!busy || conflict"
        @click="cancelLogin"
      >
        取消当前登录
      </button>
      <p class="footnote weixin-scope-note">
        扫码确认仅表示凭据保存成功。私聊接收、连接检查和手机实际可见回复分别验证；本页不承诺群聊或媒体能力。
      </p>
    </section>
  </div>
</template>

<style scoped>
.weixin-workspace {
  display: grid;
  gap: 20px;
  min-width: 0;
}
.weixin-workspace > *,
.weixin-heading > div {
  min-width: 0;
}
.weixin-heading {
  flex-wrap: wrap;
}
.weixin-heading > div {
  flex: 1 1 240px;
}
.weixin-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  margin-top: 20px;
}
.weixin-actions .button {
  white-space: normal;
}
.weixin-workspace dd,
.weixin-workspace code {
  overflow-wrap: anywhere;
}
.weixin-check {
  display: flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: 8px 12px;
  margin-top: 22px;
  font-size: 12px;
  color: var(--muted);
}
.weixin-check .footnote {
  flex-basis: 100%;
}
.weixin-confirm {
  padding: 16px;
  border: 1px solid #e4d2b1;
  border-radius: 9px;
  background: #fdf9f1;
  margin-top: 18px;
}
.weixin-confirm h3 {
  font-size: 14px;
}
.weixin-confirm p {
  margin-top: 10px;
  font-size: 12px;
  line-height: 1.9;
}
.weixin-qr {
  display: flex;
  flex-direction: column;
  align-items: center;
  margin: 20px 0;
  gap: 10px;
}
.weixin-qr img {
  display: block;
  width: min(240px, 100%);
  height: auto;
  aspect-ratio: 1;
  border: 1px solid var(--line);
  border-radius: 8px;
}
.weixin-qr p {
  font-size: 12px;
  color: var(--muted);
}
.weixin-login-state {
  font-size: 13px;
  line-height: 1.9;
  color: var(--indigo);
  margin-bottom: 8px;
}
.weixin-verification {
  margin: 20px 0;
  max-width: 380px;
}
.weixin-verification input {
  width: 100%;
  min-width: 0;
  max-width: 100%;
}
.weixin-scope-note {
  margin-top: 20px;
}
@media (max-width: 480px) {
  .weixin-actions {
    flex-direction: column;
    align-items: stretch;
  }
  .weixin-workspace .detail-list > div {
    flex-wrap: wrap;
    gap: 6px;
  }
  .weixin-workspace .detail-list dd {
    max-width: 100%;
  }
}
</style>
