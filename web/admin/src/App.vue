<script setup lang="ts">
import {
  computed,
  nextTick,
  onMounted,
  onUnmounted,
  reactive,
  ref,
  watch,
} from "vue";
import Icon from "./components/Icon.vue";
import AdminSelect from "./components/AdminSelect.vue";
import AssistantPersonality from "./components/AssistantPersonality.vue";
import RuntimeLogs from "./components/RuntimeLogs.vue";
import DiaryPanel from "./components/DiaryPanel.vue";
import RelationshipsPanel from "./components/RelationshipsPanel.vue";
import AffinitySettings from "./components/AffinitySettings.vue";
import ContextPolicyPanel from "./components/ContextPolicyPanel.vue";
import RestPanel from "./components/RestPanel.vue";
import WeixinChannel from "./components/WeixinChannel.vue";
import SkillsPanel from "./components/SkillsPanel.vue";
import SchedulePanel from "./components/SchedulePanel.vue";
import SpeechPanel from "./components/SpeechPanel.vue";
import PluginFactoryPanel from "./components/PluginFactoryPanel.vue";
import {
  api,
  ApiError,
  clearCsrfToken,
  errorMessage,
  setCsrfToken,
} from "./api";
import { formatBytes, formatDate, validateConfig } from "./validation";
import { describeQqConnection } from "./connection";
import type {
  AuthResponse,
  AuthUser,
  Check,
  ConfigResponse,
  Conversation,
  Draft,
  FieldError,
  Operation,
  Revision,
  RuntimeConfig,
  SecretMetadata,
  SystemStatus,
  SkillDraft,
} from "./types";
type Page =
  | "overview"
  | "assistant"
  | "skills"
  | "triggers"
  | "qq"
  | "speech"
  | "plugins"
  | "weixin"
  | "logs"
  | "config"
  | "memory";
type ExtendedPage = Page | "relationships";
const navigation: {
  id: ExtendedPage;
  icon: string;
  title: string;
  eyebrow: string;
  description: string;
}[] = [
  {
    id: "overview",
    icon: "grid",
    title: "运行总览",
    eyebrow: "WORKSPACE OVERVIEW",
    description: "运行状态，一目了然。",
  },
  {
    id: "assistant",
    icon: "settings",
    title: "性格",
    eyebrow: "PERSONALITY",
    description: "她的性情、说话方式与相处习惯。",
  },
  {
    id: "skills",
    icon: "bolt",
    title: "Skill 管理",
    eyebrow: "SKILLS & METHODS",
    description: "管理做事的方法，让每次任务都有章可循。",
  },
  {
    id: "triggers",
    icon: "clock",
    title: "日程",
    eyebrow: "SCHEDULE & PROGRESS",
    description: "安排日常活动与工作目标，看见每一步的真实进展。",
  },
  {
    id: "qq",
    icon: "channel",
    title: "QQ 连接",
    eyebrow: "CONNECTED CHANNELS",
    description: "通过 QQ 保持联系。",
  },
  {
    id: "weixin",
    icon: "channel",
    title: "微信连接",
    eyebrow: "WEIXIN PRIVATE CHAT",
    description: "扫码连接授权账号的微信私聊。",
  },
  {
    id: "speech",
    icon: "wave",
    title: "语音与媒体",
    eyebrow: "VOICE & MEDIA",
    description: "管理本地 Piper 中文语音和富媒体回复。",
  },
  {
    id: "plugins",
    icon: "shield",
    title: "插件工厂",
    eyebrow: "PLUGIN FACTORY",
    description: "生成、验证和管理受限 Rust / WIT 插件。",
  },
  {
    id: "logs",
    icon: "chat",
    title: "日志记录",
    eyebrow: "RUNTIME JOURNAL",
    description: "查看消息往来、能力调用与语音处理过程。",
  },
  {
    id: "memory",
    icon: "sparkles",
    title: "日记与资料",
    eyebrow: "DIARY & SOURCES",
    description: "记录日常、整理经历，让每段记忆都有来源。",
  },
  {
    id: "relationships",
    icon: "channel",
    title: "人际关系",
    eyebrow: "PEOPLE & CONNECTIONS",
    description: "记得每个人的不同，珍惜有来由的联系。",
  },
  {
    id: "config",
    icon: "settings",
    title: "配置",
    eyebrow: "CONFIGURATION",
    description: "设置模型、工作节奏与相处规则，查看配置历史。",
  },
];
const page = ref<ExtendedPage>("overview");
const activePage = computed(() =>
  navigation.find((item) => item.id === page.value)!,
);
const mobileMenu = ref(false);
const compactNavigation = ref(false);
let navigationMedia: MediaQueryList | undefined;
function syncNavigationSize() {
  compactNavigation.value = navigationMedia?.matches ?? false;
}
const sidebarExtras = ref<HTMLDetailsElement | null>(null);
watch(page, async (current, previous) => {
  if (previous === "config" && current !== previous)
    affinityLoaded.value = false;
  await nextTick();
  window.scrollTo(0, 0);
});

function dismissSidebarExtras(event: Event) {
  if (event instanceof KeyboardEvent && event.key !== "Escape") return;
  if (
    sidebarExtras.value &&
    (event instanceof KeyboardEvent ||
      !sidebarExtras.value.contains(event.target as Node))
  )
    sidebarExtras.value.open = false;
  if (event instanceof KeyboardEvent) mobileMenu.value = false;
}
const authState = ref<"loading" | "setup" | "login" | "ready" | "unavailable">(
  "loading",
);
watch(authState, async (state) => {
  if (state === "ready") {
    await nextTick();
    window.scrollTo(0, 0);
  }
});
const user = ref<AuthUser | null>(null);
const auth = reactive({
  username: "admin",
  password: "",
  confirmation: "",
  bootstrap_token: "",
});
const busy = ref("");
const error = ref("");
const notice = ref("");
const status = ref<SystemStatus | null>(null);
const statusSampledAt = ref<number | null>(null);
const statusRefreshing = ref(false);
const statusError = ref("");
let statusTimer: ReturnType<typeof setTimeout> | undefined;
let statusRequest: Promise<void> | null = null;
let statusController: AbortController | null = null;
let authEpoch = 0;
const version = ref("M1");
const frontendBuild = import.meta.env.VITE_BUILD_SHA || "local";
const brandMarkUrl = `${import.meta.env.BASE_URL}brand-mark.svg`;
const mascotUrl = `${import.meta.env.BASE_URL}mascot.png`;
const configState = ref<ConfigResponse | null>(null);
const config = reactive<RuntimeConfig>({
  assistant: {
    name: "DSH",
    system_prompt: "",
    max_concurrent_runs: 2,
    timeout_seconds: 120,
  },
  model: {
    base_url: "https://api.deepseek.com",
    model: "deepseek-chat",
    credential_ref: null,
    image_generation: false,
  },
  qq: {
    enabled: false,
    app_id: "",
    credential_ref: null,
    transport: "websocket",
  },
});
const secretMetadata = ref<SecretMetadata[]>([]);
const modelKey = ref("");
const qqSecret = ref("");
const draft = ref<Draft | null>(null);
const validated = ref(false);
const validationErrors = ref<FieldError[]>([]);
const checks = ref<Check[]>([]);
const operation = ref<Operation | null>(null);
const dirty = ref(false);
let replacingConfig = false;
const revisions = ref<Revision[]>([]);
const conversations = ref<Conversation[]>([]);
const personalityPanel = ref<InstanceType<typeof AssistantPersonality> | null>(
  null,
);
const affinityPanel = ref<InstanceType<typeof AffinitySettings> | null>(null);
const contextPolicyPanel = ref<InstanceType<typeof ContextPolicyPanel> | null>(
  null,
);
const restPanel = ref<InstanceType<typeof RestPanel> | null>(null);
const contextPolicyLoaded = ref(false);
const affinityLoaded = ref(false);
const historyLoaded = ref(false);
function openAffinity(event: Event) {
  if ((event.target as HTMLDetailsElement).open) affinityLoaded.value = true;
}
function openWorkRhythm(event: Event) {
  if ((event.target as HTMLDetailsElement).open)
    contextPolicyLoaded.value = true;
}
function openHistory(event: Event) {
  if ((event.target as HTMLDetailsElement).open)
    void task("读取历史", loadHistory);
}
const skillsPanel = ref<InstanceType<typeof SkillsPanel> | null>(null);
const skillDraft = ref<SkillDraft | null>(null);
const taskTriggers = ref<InstanceType<typeof SchedulePanel> | null>(null);
let disposed = false;
const configuredModel = computed(() => Boolean(config.model.credential_ref));
const configuredQq = computed(() => Boolean(config.qq.credential_ref));
const qqConnection = computed(() =>
  describeQqConnection(status.value, Boolean(statusError.value)),
);
const qqHasConnectionEvidence = computed(
  () =>
    qqConnection.value.connected ||
    (!statusError.value &&
      status.value?.channels?.qq?.connection_status === "webhook" &&
      Boolean(status.value.channels.qq.last_event_at)),
);
const effective = computed(
  () =>
    configState.value &&
    configState.value.revision === configState.value.applied_revision,
);
const totalMemory = computed(() => status.value?.platform?.memory_total_bytes);
const availableMemory = computed(
  () => status.value?.platform?.memory_available_bytes,
);
const memoryPercent = computed(() =>
  totalMemory.value &&
  availableMemory.value !== undefined &&
  availableMemory.value !== null
    ? Math.min(
        100,
        Math.max(0, (1 - availableMemory.value / totalMemory.value) * 100),
      )
    : 0,
);
const setupProgress = computed(
  () =>
    [
      configuredModel.value,
      qqHasConnectionEvidence.value,
      conversations.value.length > 0,
    ].filter(Boolean).length,
);

watch(
  config,
  () => {
    if (!replacingConfig) {
      dirty.value = true;
      draft.value = null;
      validated.value = false;
      checks.value = [];
      validationErrors.value = [];
    }
  },
  { deep: true, flush: "sync" },
);
watch([modelKey, qqSecret], () => {
  dirty.value = true;
  draft.value = null;
  validated.value = false;
  checks.value = [];
});
watch(
  authState,
  (value) => {
    authEpoch++;
    if (statusTimer !== undefined) clearTimeout(statusTimer);
    statusTimer = undefined;
    if (value === "ready") scheduleStatusPoll();
    else statusController?.abort();
  },
  { flush: "sync" },
);

function expireSession() {
  authState.value = "login";
  user.value = null;
  clearCsrfToken();
  modelKey.value = "";
  qqSecret.value = "";
  configState.value = null;
  status.value = null;
  statusSampledAt.value = null;
  statusError.value = "";
  conversations.value = [];
  revisions.value = [];
  historyLoaded.value = false;
  affinityLoaded.value = false;
}

function scheduleStatusPoll() {
  if (disposed || authState.value !== "ready") return;
  statusTimer = setTimeout(async () => {
    statusTimer = undefined;
    try {
      if (!document.hidden) await refreshStatus(true);
    } catch {
      // A concurrent manual refresh reports its own failure through task().
    } finally {
      scheduleStatusPoll();
    }
  }, 5000);
}

function refreshStatus(quiet = false): Promise<void> {
  if (statusRequest) return statusRequest;
  if (disposed || authState.value !== "ready") return Promise.resolve();
  const epoch = authEpoch;
  const controller = new AbortController();
  statusController = controller;
  const deadline = setTimeout(() => controller.abort(), 10_000);
  statusRefreshing.value = true;
  statusRequest = (async () => {
    try {
      const result = await api<SystemStatus>("/status", {
        signal: controller.signal,
      });
      if (disposed || epoch !== authEpoch || controller.signal.aborted) return;
      status.value = result;
      statusSampledAt.value = Date.now();
      statusError.value = "";
      version.value = result.version || version.value;
    } catch (caught) {
      if (disposed || epoch !== authEpoch) return;
      if (caught instanceof ApiError && caught.status === 401) {
        expireSession();
        error.value = "管理会话已过期，请重新登录。";
      } else
        statusError.value =
          "状态刷新未完成，以下为上次采样。请检查服务连接或手动刷新。";
      if (!quiet) throw caught;
    } finally {
      clearTimeout(deadline);
      statusRefreshing.value = false;
      statusController = null;
      statusRequest = null;
    }
  })();
  return statusRequest;
}

function mayLeavePage() {
  if (
    restPanel.value?.hasUnsaved &&
    !window.confirm("作息有未保存的填写或结果未确认的操作，确定离开？")
  )
    return false;
  if (
    contextPolicyPanel.value?.hasUnsaved &&
    !window.confirm("上下文预算有未保存的修改，确定离开？")
  )
    return false;
  if (
    affinityPanel.value?.hasUnsaved &&
    !window.confirm("好感规则有未保存的修改，确定离开？")
  )
    return false;
  if (
    personalityPanel.value?.hasUnsaved &&
    !window.confirm("性格有未保存的修改，确定离开？")
  )
    return false;
  if (
    taskTriggers.value?.hasUnsaved &&
    !window.confirm("任务有未保存修改或结果未确认的操作，确定离开？")
  )
    return false;
  if (
    skillsPanel.value?.hasUnsaved &&
    !window.confirm("Skill 有未保存的修改，确定离开？")
  )
    return false;
  return true;
}
function navigate(id: ExtendedPage) {
  if (id !== page.value && !mayLeavePage()) return;
  page.value = id;
  if (sidebarExtras.value) sidebarExtras.value.open = false;
  location.hash = id;
  mobileMenu.value = false;
  error.value = "";
}
function readHash() {
  const requested = location.hash.slice(1);
  const mapped =
    requested === "history"
      ? "config"
      : requested === "schedule"
        ? "triggers"
        : requested === "diary"
          ? "memory"
          : requested === "characters"
            ? "assistant"
            : ["chat", "diagnostics", "images", "wasm"].includes(requested)
              ? "logs"
              : requested;
  if (!navigation.some((item) => item.id === mapped)) return;
  if (mapped !== page.value && !mayLeavePage()) {
    history.replaceState(
      null,
      "",
      `${location.pathname}${location.search}#${page.value}`,
    );
    return;
  }
  page.value = mapped as ExtendedPage;
  if (mapped !== requested)
    history.replaceState(
      null,
      "",
      `${location.pathname}${location.search}#${mapped}`,
    );
}
async function task(label: string, fn: () => Promise<unknown>) {
  if (busy.value) return;
  busy.value = label;
  error.value = "";
  notice.value = "";
  try {
    return await fn();
  } catch (caught) {
    error.value = errorMessage(caught);
    if (
      caught instanceof ApiError &&
      caught.status === 401 &&
      authState.value === "ready"
    ) {
      expireSession();
    }
  } finally {
    busy.value = "";
  }
}
async function initialize() {
  authState.value = "loading";
  error.value = "";
  try {
    const authStatus = await api<{ initialized: boolean; version?: string }>(
      "/auth/status",
    );
    version.value = authStatus.version || "M1";
    if (!authStatus.initialized) {
      authState.value = "setup";
      return;
    }
    try {
      const result = await api<AuthResponse>("/auth/me");
      user.value = result.user;
      setCsrfToken(result.csrf_token);
      authState.value = "ready";
      await refresh();
    } catch (caught) {
      if (caught instanceof ApiError && caught.status === 401)
        authState.value = "login";
      else throw caught;
    }
  } catch (caught) {
    authState.value = "unavailable";
    error.value = errorMessage(caught);
  }
}
async function authenticate() {
  if (
    authState.value === "setup" &&
    !/^[A-Za-z0-9_.-]{3,64}$/.test(auth.username)
  ) {
    error.value = "用户名应为 3 到 64 个英文字母、数字、下划线、点或短横线。";
    return;
  }
  if (authState.value === "setup" && auth.password !== auth.confirmation) {
    error.value = "两次输入的密码不一致。";
    return;
  }
  await task(authState.value === "setup" ? "初始化" : "登录", async () => {
    const isSetup = authState.value === "setup";
    const result = await api<AuthResponse>(
      isSetup ? "/auth/bootstrap" : "/auth/login",
      {
        method: "POST",
        body: isSetup
          ? {
              username: auth.username,
              password: auth.password,
              bootstrap_token: auth.bootstrap_token,
            }
          : { username: auth.username, password: auth.password },
      },
    );
    auth.password = "";
    auth.confirmation = "";
    auth.bootstrap_token = "";
    user.value = result.user;
    setCsrfToken(result.csrf_token);
    authState.value = "ready";
    await refresh();
  });
}
async function logout() {
  if (
    restPanel.value?.hasUnsaved &&
    !window.confirm("作息有未保存的填写或结果未确认的操作，确定退出登录？")
  )
    return;
  if (
    contextPolicyPanel.value?.hasUnsaved &&
    !window.confirm("上下文预算有未保存的修改，确定退出登录？")
  )
    return;
  if (
    affinityPanel.value?.hasUnsaved &&
    !window.confirm("好感规则有未保存的修改，确定退出登录？")
  )
    return;
  if (
    taskTriggers.value?.hasUnsaved &&
    !window.confirm("任务有未保存修改或结果未确认的操作，确定退出登录？")
  )
    return;
  if (
    skillsPanel.value?.hasUnsaved &&
    !window.confirm("Skill 有未保存的修改，确定退出登录？")
  )
    return;
  if (
    personalityPanel.value?.hasUnsaved &&
    !window.confirm("性格有未保存的修改，确定退出登录？")
  )
    return;
  await task("退出登录", async () => {
    await api("/auth/logout", { method: "POST" });
    auth.password = "";
    auth.bootstrap_token = "";
    expireSession();
  });
}
async function loadConfig() {
  const result = await api<ConfigResponse>("/config");
  configState.value = result;
  replacingConfig = true;
  Object.assign(config.assistant, result.config.assistant);
  Object.assign(config.model, result.config.model);
  Object.assign(config.qq, result.config.qq);
  replacingConfig = false;
  dirty.value = false;
  draft.value = null;
  validated.value = false;
  checks.value = [];
  validationErrors.value = [];
  const secrets = await api<{ items: SecretMetadata[] } | SecretMetadata[]>(
    "/secrets",
  );
  secretMetadata.value = Array.isArray(secrets) ? secrets : secrets.items;
}
async function refresh() {
  const results = await Promise.allSettled([
    refreshStatus(),
    loadConfig(),
    loadConversations(),
  ]);
  for (const result of results)
    if (result.status === "rejected") throw result.reason;
}
async function loadHistory() {
  const result = await api<{ items: Revision[] }>("/config/revisions");
  revisions.value = result.items;
  historyLoaded.value = true;
}
async function saveDraft() {
  await task("保存草稿", async () => {
    if (!configState.value) return;
    if (modelKey.value.trim()) {
      const secret = await api<SecretMetadata>("/secrets", {
        method: "POST",
        body: { kind: "model_api_key", value: modelKey.value.trim() },
      });
      config.model.credential_ref = secret.id;
      modelKey.value = "";
    }
    if (qqSecret.value.trim()) {
      const secret = await api<SecretMetadata>("/secrets", {
        method: "POST",
        body: { kind: "qq_app_secret", value: qqSecret.value.trim() },
      });
      config.qq.credential_ref = secret.id;
      qqSecret.value = "";
    }
    validationErrors.value = validateConfig(config);
    if (validationErrors.value.length) return;
    draft.value = await api<Draft>("/config/drafts", {
      method: "POST",
      body: {
        base_revision: configState.value.revision,
        config: structuredClone(JSON.parse(JSON.stringify(config))),
      },
    });
    dirty.value = false;
    validated.value = false;
    checks.value = [];
    operation.value = null;
    notice.value = "草稿已保存。校验并应用后，新的配置才会生效。";
  });
}
async function validateDraft() {
  await task("校验配置", async () => {
    if (!draft.value) return;
    const result = await api<{ valid: boolean; errors: FieldError[] }>(
      `/config/drafts/${encodeURIComponent(draft.value.id)}/validate`,
      { method: "POST" },
    );
    validated.value = result.valid;
    validationErrors.value = result.errors || [];
    notice.value = result.valid ? "配置校验通过，可以进行连接测试或应用。" : "";
  });
}
async function checkDraft() {
  await task("测试连接", async () => {
    if (!draft.value) return;
    const result = await api<{ ok: boolean; checks: Check[] }>(
      `/config/drafts/${encodeURIComponent(draft.value.id)}/checks`,
      { method: "POST" },
    );
    checks.value = result.checks || [];
    notice.value = result.ok
      ? "探测通过，仅覆盖列出的模型调用或凭证鉴权。QQ WebSocket 连接与消息收发请查看实际运行状态和入站事件。"
      : "";
  });
}
async function waitOperation(value: Operation): Promise<Operation> {
  let current = value;
  for (
    let index = 0;
    index < 60 &&
    !disposed &&
    ["queued", "running", "applying", "pending"].includes(current.status);
    index++
  ) {
    await new Promise((resolve) => setTimeout(resolve, 1000));
    current = await api<Operation>(
      `/operations/${encodeURIComponent(value.id)}`,
    );
    operation.value = current;
  }
  return current;
}
async function applyDraft() {
  await task("应用配置", async () => {
    if (!draft.value || !configState.value) return;
    operation.value = await waitOperation(
      await api<Operation>(
        `/config/drafts/${encodeURIComponent(draft.value.id)}/apply`,
        { method: "POST", revision: configState.value.revision },
      ),
    );
    if (operation.value.status === "completed") {
      await refresh();
      if (historyLoaded.value) await loadHistory();
      notice.value = config.qq.enabled
        ? "配置已生效。QQ 连接与收发结果仍以运行状态及入站事件为准。"
        : "配置已生效。新的对话将使用这一版本。";
    } else if (operation.value.status === "pending_restart")
      notice.value = "配置已保存，等待按部署流程重启服务。";
    else
      error.value =
        operation.value.error || "应用尚未完成，请在配置历史中检查实际版本。";
  });
}
async function rollback(revision: number) {
  if (
    !window.confirm(
      `恢复到配置版本 v${revision}？此操作会创建一次新的配置应用记录。`,
    )
  )
    return;
  await task("恢复配置", async () => {
    if (!configState.value) return;
    operation.value = await waitOperation(
      await api<Operation>(`/config/revisions/${revision}/rollback`, {
        method: "POST",
        revision: configState.value.revision,
      }),
    );
    await refresh();
    await loadHistory();
    if (operation.value.status === "completed")
      notice.value = `已恢复配置 v${revision} 的内容。`;
    else
      error.value = operation.value.error || "恢复尚未完成，请查看应用状态。";
  });
}
async function loadConversations() {
  const result = await api<{ items: Conversation[] }>("/conversations");
  conversations.value = result.items;
}
onMounted(() => {
  navigationMedia = window.matchMedia("(max-width: 720px)");
  syncNavigationSize();
  navigationMedia.addEventListener("change", syncNavigationSize);
  readHash();
  window.addEventListener("hashchange", readHash);
  document.addEventListener("pointerdown", dismissSidebarExtras);
  document.addEventListener("keydown", dismissSidebarExtras);
  void initialize();
});
onUnmounted(() => {
  disposed = true;
  if (statusTimer !== undefined) clearTimeout(statusTimer);
  statusController?.abort();
  window.removeEventListener("hashchange", readHash);
  document.removeEventListener("pointerdown", dismissSidebarExtras);
  document.removeEventListener("keydown", dismissSidebarExtras);
  navigationMedia?.removeEventListener("change", syncNavigationSize);
  clearCsrfToken();
});
</script>

<template>
  <div v-if="authState !== 'ready'" class="auth-shell">
    <section class="auth-story">
      <a href="/admin/" class="brand auth-brand"
        ><img class="brand-mark" :src="brandMarkUrl" alt="DSH 鲸鱼标志" />
        <strong>DSH<span>日常空间</span></strong></a
      >
      <div class="story-content">
        <span class="eyebrow light">A QUIET COMPANION, ALWAYS NEAR.</span>
        <h1>山有嘉卉，<br />日子温柔生长。</h1>
        <p>刘嘉卉，温柔而有主见。<br />一起把平常的日子，过得有声有色。</p>
        <div class="story-portrait">
          <span class="portrait-halo" aria-hidden="true"></span>
          <Icon class="portrait-spark" name="sparkles" :size="30" />
          <img
            class="mascot-image"
            :src="mascotUrl"
            alt="蓝发蓝眼、身着白色蕾丝与深蓝裙装的鲸鱼主题插画"
            width="320"
            height="360"
          />
          <span class="portrait-caption"
            ><Icon name="bow" :size="18" /> YOUR EVERYDAY COMPANION</span
          >
        </div>
      </div>
      <div class="story-footer">
        <span class="status-dot"></span> 为 Raspberry Pi 5 设计
        <span class="story-divider">/</span> Rust · 轻量 · 可掌控
      </div>
    </section>
    <main class="auth-main">
      <div v-if="authState === 'loading'" class="auth-form centered">
        <span class="spinner big"></span>
        <h2>正在打开日常空间</h2>
        <p class="muted">读取服务与登录状态…</p>
      </div>
      <div v-else-if="authState === 'unavailable'" class="auth-form">
        <span class="section-icon"><Icon name="channel" /></span>
        <h2>暂时无法连接</h2>
        <p class="muted">{{ error }}</p>
        <button class="button primary full" @click="initialize">
          <Icon name="refresh" />重新连接
        </button>
        <p class="footnote">
          请确认 DSH 服务正在运行，并已连接 SSH 隧道或受保护的 HTTPS 入口。
        </p>
      </div>
      <form v-else class="auth-form" @submit.prevent="authenticate">
        <span class="auth-emblem"><Icon name="whale" :size="28" /></span>
        <span class="eyebrow">{{
          authState === "setup" ? "WELCOME TO YOUR WORKSPACE" : "WELCOME BACK"
        }}</span>
        <h2>{{ authState === "setup" ? "建立你的管理账户" : "欢迎回来" }}</h2>
        <p class="auth-subtitle">
          {{
            authState === "setup"
              ? "完成首次设置，认识刘嘉卉。"
              : "登录空间，接着写下你们的日常。"
          }}
        </p>
        <div v-if="error" class="alert error" role="alert">{{ error }}</div>
        <label v-if="authState === 'setup'" class="field"
          ><span>一次性配对凭证</span
          ><input
            v-model="auth.bootstrap_token"
            type="password"
            required
            autocomplete="off"
            placeholder="粘贴从 SSH 终端获取的凭证"
          /><small
            >在树莓派终端运行 bootstrap-token
            命令获取，仅用于首次初始化。</small
          ></label
        >
        <label class="field"
          ><span>管理员用户名</span
          ><input
            v-model="auth.username"
            required
            autocomplete="username"
            maxlength="64"
            placeholder="输入用户名"
        /></label>
        <label class="field"
          ><span>{{ authState === "setup" ? "设置管理密码" : "管理密码" }}</span
          ><input
            v-model="auth.password"
            type="password"
            :minlength="authState === 'setup' ? 12 : undefined"
            required
            :autocomplete="
              authState === 'setup' ? 'new-password' : 'current-password'
            "
            :placeholder="
              authState === 'setup' ? '至少 12 个字符' : '输入管理密码'
            "
        /></label>
        <label v-if="authState === 'setup'" class="field"
          ><span>再次输入密码</span
          ><input
            v-model="auth.confirmation"
            type="password"
            minlength="12"
            required
            autocomplete="new-password"
            placeholder="确认管理密码"
        /></label>
        <button class="button primary full" type="submit" :disabled="!!busy">
          <span v-if="busy" class="spinner"></span
          >{{
            busy
              ? "正在处理…"
              : authState === "setup"
                ? "创建账户并进入控制台"
                : "登录控制台"
          }}<Icon v-if="!busy" name="arrow" :size="18" />
        </button>
        <p class="auth-note">
          <Icon name="shield" :size="16" />管理账户独立于树莓派的 SSH 登录账户。
        </p>
      </form>
      <span class="auth-version">DSH · {{ version }}</span>
    </main>
  </div>

  <div v-else class="workspace">
    <div
      v-if="mobileMenu"
      class="sidebar-backdrop"
      @click="mobileMenu = false"
    ></div>
    <aside
      class="sidebar"
      :class="{ open: mobileMenu }"
      :inert="compactNavigation && !mobileMenu"
      :aria-hidden="compactNavigation && !mobileMenu ? true : undefined"
      aria-label="工作空间导航"
    >
      <div class="sidebar-header">
        <a href="#overview" class="brand" @click.prevent="navigate('overview')"
          ><img class="brand-mark" :src="brandMarkUrl" alt="DSH 鲸鱼标志" />
          <strong>DSH<span>日常空间</span></strong></a
        >
        <button
          class="icon-button sidebar-close"
          aria-label="关闭导航"
          @click="mobileMenu = false"
        >
          <Icon name="close" :size="18" />
        </button>
      </div>
      <div class="sidebar-main">
        <div class="workspace-label">
          个人工作空间 <span class="tiny-badge">自托管</span>
        </div>
        <nav aria-label="主导航">
          <button
            v-for="item in navigation"
            :key="item.id"
            :class="{ active: page === item.id }"
            :aria-current="page === item.id ? 'page' : undefined"
            @click="navigate(item.id)"
          >
            <Icon :name="item.icon" /><span>{{ item.title }}</span
            ><span
              v-if="item.id === 'qq' && status?.channels?.qq?.enabled"
              class="nav-dot"
            ></span>
          </button>
        </nav>
      </div>
      <div class="sidebar-footer">
        <details ref="sidebarExtras" class="sidebar-extras">
          <summary>
            <Icon name="sparkles" :size="18" />
            <span>更多能力<small>按权限开放</small></span>
            <Icon class="extras-chevron" name="chevron" :size="14" />
          </summary>
          <div class="sidebar-extras-content">
            <span class="eyebrow">COMING NEXT</span>
            <strong>下一段旅程</strong>
            <p>语音识别与更多插件权限会在验证后逐步开放。</p>
            <div class="companion-note">
              <Icon name="whale" :size="24" />
              <span>让每一次对话，<br />都有温柔的回响。</span>
            </div>
          </div>
        </details>
        <div class="device-card">
          <div class="device-icon"><Icon name="cpu" /></div>
          <div>
            <strong>Raspberry Pi 5</strong
            ><span>8GB · Ubuntu 24.04 LTS 64 位 / aarch64</span>
          </div>
          <span class="status-dot"></span>
        </div>
        <div class="sidebar-user">
          <div class="avatar">
            {{ user?.username?.slice(0, 1).toUpperCase() }}
          </div>
          <div>
            <strong>{{ user?.username }}</strong
            ><span>{{
              user?.role === "admin" ? "实例管理员" : user?.role
            }}</span>
          </div>
          <button
            class="icon-button"
            aria-label="退出登录"
            title="退出登录"
            :disabled="!!busy"
            @click="logout"
          >
            <Icon name="logout" :size="18" />
          </button>
        </div>
      </div>
    </aside>

    <div class="main-wrap">
      <header class="topbar">
        <div class="breadcrumbs">
          <button
            class="icon-button mobile-trigger"
            aria-label="打开导航"
            :aria-expanded="mobileMenu"
            @click="mobileMenu = true"
          >
            <Icon name="menu" /></button
          ><span>工作空间</span><Icon name="chevron" :size="13" /><strong>{{
            activePage.title
          }}</strong>
        </div>
        <div class="topbar-right">
          <span class="connection-state"
            ><span class="status-dot" :class="{ amber: !!statusError }"></span
            >{{ statusError ? "状态刷新失败" : "管理服务已连接" }}</span
          ><span class="version-tag">{{ version }}</span
          ><button
            class="icon-button"
            aria-label="刷新运行状态"
            title="刷新运行状态"
            :disabled="!!busy || statusRefreshing"
            @click="task('刷新状态', () => refreshStatus())"
          >
            <Icon name="refresh" :size="18" />
          </button>
        </div>
      </header>
      <main class="content">
        <div class="page-heading">
          <div>
            <span class="eyebrow">{{ activePage.eyebrow }}</span>
            <h1>{{ activePage.title }}</h1>
            <p>{{ activePage.description }}</p>
          </div>
          <span
            v-if="['overview', 'qq', 'config'].includes(page) && configState"
            class="revision-label"
            ><span class="status-dot" :class="{ amber: !effective }"></span
            >{{ effective ? "当前配置已生效" : "配置等待生效"
            }}<b>v{{ configState.applied_revision }}</b></span
          >
        </div>
        <div v-if="error" class="alert error" role="alert">
          <span>{{ error }}</span
          ><button
            class="icon-button"
            aria-label="关闭错误提示"
            @click="error = ''"
          >
            <Icon name="close" :size="16" />
          </button>
        </div>
        <div v-if="notice" class="alert success" role="status">
          <Icon name="check" :size="18" /><span>{{ notice }}</span
          ><button
            class="icon-button"
            aria-label="关闭提示"
            @click="notice = ''"
          >
            <Icon name="close" :size="16" />
          </button>
        </div>
        <div v-if="busy" class="activity-bar" role="status">
          <span class="spinner"></span>{{ busy }}…
        </div>

        <template v-if="page === 'overview'">
          <section class="welcome-card">
            <div>
              <span class="eyebrow light"
                ><Icon name="bow" :size="17" /> YOUR LITTLE HARBOR</span
              >
              <h2>今天，也一起吧。</h2>
              <p>
                {{
                  configuredModel
                    ? "模型已配置，可在日志中查看消息与运行情况。"
                    : "连接一个模型，让日常的交流从这里开始。"
                }}
              </p>
              <button
                class="button white"
                @click="navigate(configuredModel ? 'logs' : 'config')"
              >
                {{ configuredModel ? "查看运行日志" : "配置第一个模型"
                }}<Icon name="arrow" :size="17" />
              </button>
            </div>
            <div class="welcome-art">
              <span class="welcome-halo" aria-hidden="true"></span>
              <Icon class="welcome-spark" name="sparkles" :size="26" />
              <img
                class="mascot-image"
                :src="mascotUrl"
                alt="刘嘉卉的鲸鱼主题插画"
                width="240"
                height="270"
              />
              <span class="welcome-signature" aria-hidden="true"
                >DSH · by your side</span
              >
            </div>
          </section>
          <section class="metrics" aria-label="运行指标">
            <article class="metric">
              <div class="metric-title">
                活跃任务<Icon name="bolt" :size="17" />
              </div>
              <strong
                >{{ status?.queue?.running ?? "—"
                }}<span
                  >/ {{ config.assistant.max_concurrent_runs }}</span
                ></strong
              >
              <p>{{ status?.queue?.queued ?? 0 }} 个任务等待处理</p>
            </article>
            <article class="metric">
              <div class="metric-title">
                系统内存<Icon name="cpu" :size="17" />
              </div>
              <strong
                >{{ availableMemory ? formatBytes(availableMemory) : "—"
                }}<span>可用</span></strong
              >
              <p>设备总量 {{ formatBytes(totalMemory) }}</p>
              <div class="memory-track">
                <i :style="{ width: `${memoryPercent}%` }"></i>
              </div>
            </article>
            <article class="metric">
              <div class="metric-title">
                模型连接<Icon name="settings" :size="17" />
              </div>
              <strong class="word-metric"
                >{{ status?.model?.configured ? "已配置" : "待配置"
                }}<span
                  class="status-dot"
                  :class="{ amber: !status?.model?.configured }"
                ></span
              ></strong>
              <p class="ellipsis">
                {{ status?.model?.model || "等待选择模型" }}
              </p>
            </article>
            <article class="metric">
              <div class="metric-title">
                QQ 渠道<Icon name="channel" :size="17" />
              </div>
              <strong class="word-metric"
                >{{ qqConnection.short
                }}<span
                  class="status-dot"
                  :class="{ amber: !qqConnection.connected }"
                ></span
              ></strong>
              <p>
                {{
                  status?.channels?.qq?.last_event_at
                    ? `最近事件 ${formatDate(status.channels.qq.last_event_at)}`
                    : "尚无入站事件"
                }}
              </p>
            </article>
          </section>
          <div class="overview-columns">
            <section class="panel">
              <div class="panel-heading">
                <div>
                  <h2>从这里开始</h2>
                  <p>完成这几步，建立第一个工作闭环。</p>
                </div>
                <span class="progress-pill">{{ setupProgress }} / 3</span>
              </div>
              <div class="setup-list">
                <button @click="navigate('config')">
                  <span class="step-number" :class="{ done: configuredModel }"
                    ><Icon
                      v-if="configuredModel"
                      name="check"
                      :size="16"
                    /><template v-else>01</template></span
                  ><span
                    ><strong>连接模型</strong
                    ><small>填写模型地址、名称与 API Key</small></span
                  ><Icon name="chevron" :size="18" /></button
                ><button @click="navigate('logs')">
                  <span
                    class="step-number"
                    :class="{ done: conversations.length > 0 }"
                    ><Icon
                      v-if="conversations.length"
                      name="check"
                      :size="16"
                    /><template v-else>02</template></span
                  ><span
                    ><strong>查看运行日志</strong
                    ><small>查看渠道输入、模型输出与执行过程</small></span
                  ><Icon name="chevron" :size="18" /></button
                ><button @click="navigate('qq')">
                  <span
                    class="step-number"
                    :class="{ done: qqHasConnectionEvidence }"
                    ><Icon
                      v-if="qqHasConnectionEvidence"
                      name="check"
                      :size="16"
                    /><template v-else>03</template></span
                  ><span
                    ><strong>接入 QQ</strong
                    ><small>配置 App ID、凭证与连接方式</small></span
                  ><Icon name="chevron" :size="18" />
                </button>
              </div>
            </section>
            <section class="panel runtime-panel">
              <div class="panel-heading">
                <div>
                  <h2>运行环境</h2>
                  <p>设备端实时状态</p>
                </div>
                <span class="soft-icon"><Icon name="shield" /></span>
              </div>
              <dl class="detail-list">
                <div>
                  <dt>操作系统</dt>
                  <dd>
                    {{ status?.platform?.os || "等待采样" }} /
                    {{ status?.platform?.arch || "—" }}
                  </dd>
                </div>
                <div>
                  <dt>运行时长</dt>
                  <dd>
                    {{
                      status?.uptime_seconds !== undefined
                        ? `${Math.floor(status.uptime_seconds / 60)} 分钟`
                        : "—"
                    }}
                  </dd>
                </div>
                <div>
                  <dt>配置版本</dt>
                  <dd>v{{ configState?.applied_revision ?? "—" }}</dd>
                </div>
                <div>
                  <dt>部署提交</dt>
                  <dd class="mono">
                    {{ status?.commit?.slice(0, 10) || "未提供" }}
                  </dd>
                </div>
                <div>
                  <dt>待发送消息</dt>
                  <dd>{{ status?.queue?.outbox_pending ?? 0 }}</dd>
                </div>
              </dl>
              <p class="runtime-note">
                支持逐句回复、长期记忆、图片理解和本地 Piper
                语音；渠道实际收发结果以联调记录为准。
              </p>
            </section>
          </div>
        </template>

        <AssistantPersonality
          v-if="page === 'assistant'"
          ref="personalityPanel"
          @unauthorized="expireSession"
        />
        <template v-if="page === 'config' || page === 'qq'">
          <div class="settings-layout">
            <div class="settings-main">
              <details v-if="page === 'config'" class="panel config-drawer">
                <summary>
                  <span>模型配置</span><Icon name="chevron" :size="18" />
                </summary>
                <div class="config-drawer-body">
                  <div class="panel-heading">
                    <div>
                      <p>连接兼容 Chat Completions 的模型 API。</p>
                    </div>
                    <span
                      class="badge"
                      :class="configuredModel ? 'green' : 'neutral'"
                      >{{ configuredModel ? "已有凭证" : "未设置凭证" }}</span
                    >
                  </div>
                  <label class="field"
                    ><span>API 基础地址</span
                    ><input
                      v-model="config.model.base_url"
                      type="url"
                      placeholder="https://api.deepseek.com"
                      spellcheck="false"
                    /><small>填写服务基础地址，由服务端构造请求。</small></label
                  ><label class="field"
                    ><span>模型标识</span
                    ><input
                      v-model="config.model.model"
                      placeholder="填写服务商提供的模型名称"
                      spellcheck="false" /></label
                  ><label class="field"
                    ><span
                      >API Key
                      <span class="optional">{{
                        configuredModel ? "留空则保持原值" : "必填以启用对话"
                      }}</span></span
                    ><input
                      v-model="modelKey"
                      type="password"
                      autocomplete="new-password"
                      placeholder="输入新的 API Key"
                    /><small
                      >密钥仅用于替换，不会再次显示或保存在浏览器中。</small
                    ></label
                  >
                </div>
              </details>
              <details
                v-if="page === 'config'"
                class="panel config-drawer"
                @toggle="openWorkRhythm"
              >
                <summary>
                  <span>工作节奏</span><Icon name="chevron" :size="18" />
                </summary>
                <div class="config-drawer-body">
                  <div class="panel-heading">
                    <div>
                      <p>设置回复间隔、并发和超时。</p>
                    </div>
                  </div>
                  <div v-if="config.reply" class="field-row">
                    <label class="field"
                      ><span>回复最小间隔（毫秒）</span
                      ><input
                        v-model.number="config.reply.min_interval_ms"
                        type="number"
                        min="1000"
                        max="10000"
                      /><small
                        >逐句发送，从上一条实际发送完成时开始计时。</small
                      ></label
                    >
                    <label class="field"
                      ><span>回复最大间隔（毫秒）</span
                      ><input
                        v-model.number="config.reply.max_interval_ms"
                        type="number"
                        :min="config.reply.min_interval_ms"
                        max="30000"
                    /></label>
                    <label class="field"
                      ><span>每轮最多消息</span
                      ><input
                        v-model.number="config.reply.max_items"
                        type="number"
                        min="1"
                        max="4"
                      /><small>QQ C2C 被动回复最多 4 条。</small></label
                    >
                  </div>
                  <div class="field-row">
                    <label class="field"
                      ><span>最大活跃任务数</span
                      ><AdminSelect
                        v-model="config.assistant.max_concurrent_runs"
                        aria-label="最大活跃任务数"
                        :options="[
                          { value: 1, label: '1 个任务' },
                          { value: 2, label: '2 个任务' },
                        ]"
                      /><small>为树莓派 8GB 内存保留余量。</small></label
                    ><label class="field"
                      ><span>任务超时（秒）</span
                      ><input
                        v-model.number="config.assistant.timeout_seconds"
                        type="number"
                        min="5"
                        max="300"
                    /></label>
                  </div>
                  <ContextPolicyPanel
                    v-if="contextPolicyLoaded"
                    ref="contextPolicyPanel"
                    @unauthorized="expireSession"
                  />
                  <RestPanel
                    v-if="contextPolicyLoaded"
                    ref="restPanel"
                    @unauthorized="expireSession"
                  />
                </div>
              </details>
              <details v-if="page === 'config'" class="panel config-drawer">
                <summary>
                  <span>绘画能力</span><Icon name="chevron" :size="18" />
                </summary>
                <div class="config-drawer-body">
                  <label class="field"
                    ><span
                      ><input
                        v-model="config.model.image_generation"
                        type="checkbox"
                      />
                      当前模型支持原生生图接口</span
                    ><small
                      >仅在服务商确认当前模型支持兼容的 images/generations
                      接口和 Base64 PNG
                      返回时启用。与上面的模型共用地址、标识和凭证；未启用时，绘画
                      Skill 自动使用 HTML /
                      SVG。勾选本身不代表连接测试通过。</small
                    ></label
                  >
                </div>
              </details>
              <details
                v-if="page === 'config'"
                class="panel config-drawer"
                @toggle="openAffinity"
              >
                <summary>
                  <span>好感规则配置</span><Icon name="chevron" :size="18" />
                </summary>
                <div class="config-drawer-body">
                  <AffinitySettings
                    v-if="affinityLoaded"
                    ref="affinityPanel"
                    @unauthorized="expireSession"
                  />
                </div>
              </details>
              <details
                v-if="page === 'config'"
                class="panel config-drawer"
                @toggle="openHistory"
              >
                <summary>
                  <span>配置历史</span><Icon name="chevron" :size="18" />
                </summary>
                <div class="config-drawer-body history-panel">
                  <div class="panel-heading">
                    <div>
                      <h2>配置版本</h2>
                      <p>每次应用都有记录。恢复旧内容会生成新的配置版本。</p>
                    </div>
                    <button
                      class="button secondary small"
                      :disabled="!!busy"
                      @click="task('读取历史', loadHistory)"
                    >
                      <Icon name="refresh" :size="16" />刷新
                    </button>
                  </div>
                  <div v-if="!revisions.length" class="empty-state">
                    <Icon name="clock" :size="34" />
                    <h3>还没有版本记录</h3>
                    <p>应用一次配置后，会在这里保留历史。</p>
                  </div>
                  <div
                    v-for="revision in revisions"
                    :key="revision.revision"
                    class="revision-row"
                  >
                    <div class="revision-icon">
                      <Icon name="clock" :size="19" />
                    </div>
                    <div class="revision-detail">
                      <div>
                        <strong>配置 v{{ revision.revision }}</strong
                        ><span
                          v-if="
                            revision.current ||
                            revision.revision === configState?.applied_revision
                          "
                          class="badge green"
                          >当前生效</span
                        >
                      </div>
                      <p>
                        {{ formatDate(revision.created_at)
                        }}<span v-if="revision.config?.model?.model">
                          · {{ revision.config.model.model }}</span
                        >
                      </p>
                    </div>
                    <button
                      class="button secondary small"
                      :disabled="
                        !!busy ||
                        revision.current ||
                        revision.revision === configState?.applied_revision
                      "
                      @click="rollback(revision.revision)"
                    >
                      恢复此版本
                    </button>
                  </div>
                </div>
              </details>
              <section v-if="page === 'qq'" class="panel">
                <div class="panel-heading">
                  <div>
                    <h2>QQ 官方机器人</h2>
                    <p>接收 QQ 消息，并通过配置的模型回复。</p>
                  </div>
                  <label class="toggle"
                    ><input
                      v-model="config.qq.enabled"
                      type="checkbox"
                      aria-label="启用 QQ 机器人" /><span></span
                  ></label>
                </div>
                <label class="field"
                  ><span>接入方式</span
                  ><AdminSelect
                    v-model="config.qq.transport"
                    aria-label="接入方式"
                    :options="[
                      {
                        value: 'websocket',
                        label: 'WebSocket · 主动连接，无需公网回调',
                      },
                      { value: 'webhook', label: 'Webhook · HTTPS 回调' },
                    ]"
                  /><small
                    >QQ 开放平台的接入方式需与此处保持一致。</small
                  ></label
                ><label class="field"
                  ><span>App ID</span
                  ><input
                    v-model="config.qq.app_id"
                    placeholder="QQ 开放平台的应用 ID"
                    autocomplete="off" /></label
                ><label class="field"
                  ><span
                    >App Secret
                    <span class="optional">{{
                      configuredQq ? "已配置，留空保持原值" : "尚未配置"
                    }}</span></span
                  ><input
                    v-model="qqSecret"
                    type="password"
                    autocomplete="new-password"
                    placeholder="输入新的 App Secret"
                  /><small>密钥保存在设备端，不会回传明文。</small></label
                >
                <div
                  v-if="config.qq.transport === 'websocket'"
                  class="info-box"
                >
                  <Icon name="channel" :size="19" />
                  <div>
                    <strong>设备主动建立连接</strong>
                    <p>
                      适合树莓派单机部署。请在 QQ 开放平台选择 WebSocket
                      方式，应用配置后由设备连接官方网关。
                    </p>
                  </div>
                </div>
                <div v-else class="info-box">
                  <Icon name="channel" :size="19" />
                  <div>
                    <strong>Webhook 回调路径</strong
                    ><code>{{
                      status?.channels?.qq?.webhook_path || "/webhooks/qq"
                    }}</code>
                    <p>
                      在 QQ 开放平台配置可达的 HTTPS
                      回调地址。公开业务回调与管理入口应分别保护。
                    </p>
                  </div>
                </div>
                <div
                  v-if="status?.channels?.qq?.last_error"
                  class="alert error"
                >
                  {{ status.channels.qq.last_error }}
                </div>
                <div class="panel-heading">
                  <div>
                    <h3>实际运行状态</h3>
                    <p>
                      每 5 秒采样。凭证鉴权成功和配置生效不代表消息链路已连通。
                    </p>
                  </div>
                  <button
                    class="button secondary small"
                    :disabled="!!busy || statusRefreshing"
                    @click="task('刷新状态', () => refreshStatus())"
                  >
                    <Icon name="refresh" :size="16" />刷新状态
                  </button>
                </div>
                <p v-if="statusError" class="alert error" role="status">
                  {{ statusError }}
                </p>
                <dl class="detail-list">
                  <div>
                    <dt>当前启用状态</dt>
                    <dd>
                      {{ status?.channels?.qq?.enabled ? "已启用" : "未启用" }}
                    </dd>
                  </div>
                  <div>
                    <dt>连接状态</dt>
                    <dd data-testid="qq-connection-status">
                      {{ qqConnection.label }}
                    </dd>
                  </div>
                  <div>
                    <dt>最近入站事件</dt>
                    <dd>
                      {{ formatDate(status?.channels?.qq?.last_event_at) }}
                    </dd>
                  </div>
                  <div>
                    <dt>状态采样时间</dt>
                    <dd data-testid="status-sampled-at">
                      {{ formatDate(statusSampledAt) }}
                    </dd>
                  </div>
                </dl>
              </section>
              <section
                v-if="validationErrors.length"
                class="panel validation-panel"
                role="alert"
              >
                <h3>请检查这些配置</h3>
                <ul>
                  <li v-for="(item, index) in validationErrors" :key="index">
                    <code>{{ item.field }}</code
                    >{{ item.message }}
                  </li>
                </ul>
              </section>
              <section v-if="checks.length" class="panel">
                <div class="panel-heading">
                  <div>
                    <h2>凭证与模型探测结果</h2>
                    <p>
                      只验证列出的调用或鉴权，不证明 QQ WebSocket
                      连接和消息收发正常。
                    </p>
                  </div>
                </div>
                <div
                  v-for="(check, index) in checks"
                  :key="index"
                  class="check-result"
                >
                  <span class="check-state" :class="{ failed: !check.ok }"
                    ><Icon :name="check.ok ? 'check' : 'close'" :size="16"
                  /></span>
                  <div>
                    <strong>{{ check.name }}</strong>
                    <p>{{ check.message }}</p>
                  </div>
                </div>
              </section>
            </div>
            <aside class="settings-summary panel">
              <span class="eyebrow">CONFIGURATION FLOW</span>
              <h2>配置工作流</h2>
              <p class="muted">保存、验证，再让新的配置生效。</p>
              <ol class="flow-list">
                <li :class="{ complete: !!draft }">
                  <span>1</span>
                  <div>
                    <strong>保存草稿</strong
                    ><small>{{
                      dirty
                        ? "有尚未保存的修改"
                        : draft
                          ? "草稿已保存"
                          : "编辑需要调整的配置"
                    }}</small>
                  </div>
                </li>
                <li :class="{ complete: validated }">
                  <span>2</span>
                  <div>
                    <strong>校验与测试</strong
                    ><small>{{
                      validated ? "校验通过" : "检查字段与连接情况"
                    }}</small>
                  </div>
                </li>
                <li :class="{ complete: operation?.status === 'completed' }">
                  <span>3</span>
                  <div>
                    <strong>应用到运行环境</strong
                    ><small>保存成功不等于已经生效</small>
                  </div>
                </li>
              </ol>
              <button
                class="button primary full"
                :disabled="!!busy || !configState"
                @click="saveDraft"
              >
                <Icon name="save" :size="17" />保存草稿
              </button>
              <div class="button-row">
                <button
                  class="button secondary"
                  :disabled="!!busy || !draft"
                  @click="validateDraft"
                >
                  校验配置</button
                ><button
                  class="button secondary"
                  :disabled="!!busy || !draft"
                  @click="checkDraft"
                >
                  测试连接
                </button>
              </div>
              <button
                class="button outline full"
                :disabled="!!busy || !draft || !validated"
                @click="applyDraft"
              >
                <Icon name="bolt" :size="17" />应用配置
              </button>
              <div class="summary-bottom">
                <span>当前生效</span
                ><strong>v{{ configState?.applied_revision ?? "—" }}</strong
                ><button
                  class="link-button"
                  :disabled="!!busy"
                  @click="task('重新载入', loadConfig)"
                >
                  重新载入
                </button>
              </div>
              <p class="footnote">
                重新载入将放弃尚未保存的表单修改。版本冲突时请载入最新配置后重新编辑。
              </p>
            </aside>
          </div>
        </template>

        <SkillsPanel
          v-if="page === 'skills'"
          ref="skillsPanel"
          :initial-draft="skillDraft"
          @consumed="skillDraft = null"
          @unauthorized="expireSession"
        />
        <SchedulePanel
          v-if="page === 'triggers'"
          ref="taskTriggers"
          @unauthorized="expireSession"
          @open-diary="navigate('memory')"
        />

        <RuntimeLogs v-if="page === 'logs'" @unauthorized="expireSession" />
        <div v-if="page === 'memory'" class="memory-page">
          <DiaryPanel @unauthorized="expireSession" />
        </div>
        <RelationshipsPanel
          v-if="page === 'relationships'"
          @unauthorized="expireSession"
        />
        <WeixinChannel v-if="page === 'weixin'" />
        <section v-if="page === 'speech'" class="panel">
          <SpeechPanel />
        </section>
        <section v-if="page === 'plugins'" class="panel">
          <PluginFactoryPanel @unauthorized="expireSession" />
        </section>
        <footer class="page-footer">
          <span>DSH · 你的日常，由你安排</span
          ><span title="前端构建提交"
            >Web {{ frontendBuild.slice(0, 10) }} · Rust 服务</span
          >
        </footer>
      </main>
    </div>
  </div>
</template>
