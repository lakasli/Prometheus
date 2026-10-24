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
import CharacterCards from "./components/CharacterCards.vue";
import MemoryPanel from "./components/MemoryPanel.vue";
import ConversationSummary from "./components/ConversationSummary.vue";
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
  ChatMessage,
  Check,
  CharacterCardList,
  ConfigResponse,
  Conversation,
  Draft,
  FieldError,
  Operation,
  Revision,
  RuntimeConfig,
  Run,
  SecretMetadata,
  SystemStatus,
} from "./types";

type Page =
  | "overview"
  | "assistant"
  | "characters"
  | "qq"
  | "chat"
  | "history"
  | "memory";
const navigation: {
  id: Page;
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
    description: "助手的运行状态，一目了然。",
  },
  {
    id: "assistant",
    icon: "settings",
    title: "助手与模型",
    eyebrow: "ASSISTANT & MODELS",
    description: "为你的助手选择模型、个性和工作节奏。",
  },
  {
    id: "characters",
    icon: "sparkles",
    title: "角色卡",
    eyebrow: "CHARACTER CARDS",
    description: "设计助手的身份与表达，为新会话选择熟悉的角色。",
  },
  {
    id: "qq",
    icon: "channel",
    title: "QQ 连接",
    eyebrow: "CONNECTED CHANNELS",
    description: "把助手连接到你的 QQ 机器人。",
  },
  {
    id: "chat",
    icon: "chat",
    title: "对话工作台",
    eyebrow: "CONVERSATIONS",
    description: "直接与助手对话，验证模型配置。",
  },
  {
    id: "memory",
    icon: "sparkles",
    title: "助手记忆",
    eyebrow: "ASSISTANT MEMORY",
    description: "管理助手自己的长期记忆，保留经历与来源。",
  },
  {
    id: "history",
    icon: "clock",
    title: "配置历史",
    eyebrow: "CONFIGURATION HISTORY",
    description: "追踪每次配置变化，恢复可用版本。",
  },
];
const page = ref<Page>("overview");
const activePage = computed(() =>
  navigation.find((item) => item.id === page.value)!,
);
const mobileMenu = ref(false);
const authState = ref<"loading" | "setup" | "login" | "ready" | "unavailable">(
  "loading",
);
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
const characterCards = ref<InstanceType<typeof CharacterCards> | null>(null);
const characterOptions = ref<CharacterCardList | null>(null);
const characterChoice = ref("");
const selectedConversation = ref<string | null>(null);
const selectedConversationData = computed(() =>
  conversations.value.find((item) => item.id === selectedConversation.value),
);
const conversationAssistantName = computed(
  () =>
    selectedConversationData.value?.character?.name || config.assistant.name,
);
const conversationCharacterLabel = computed(() =>
  selectedConversationData.value?.test ||
  selectedConversationData.value?.is_test ||
  selectedConversationData.value?.character?.is_test
    ? "草稿测试会话"
    : selectedConversationData.value?.character
      ? `角色 v${selectedConversationData.value.character.revision}`
      : "原有助手",
);
const messages = ref<ChatMessage[]>([]);
const chatInput = ref("");
const chatRunning = ref(false);
const chatRun = ref<Run | null>(null);
const messageList = ref<HTMLElement | null>(null);
let disposed = false;

const configuredModel = computed(() => Boolean(config.model.credential_ref));
const memoryCommand = computed(() =>
  /^\/memory(?:\s|$)/.test(chatInput.value.trim()),
);
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
  messages.value = [];
  conversations.value = [];
  selectedConversation.value = null;
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

function navigate(id: Page) {
  if (
    id !== page.value &&
    characterCards.value?.hasUnsaved &&
    !window.confirm("角色卡有未保存的修改，确定离开？")
  )
    return;
  page.value = id;
  location.hash = id;
  mobileMenu.value = false;
  error.value = "";
  if (id === "history") void task("读取历史", loadHistory);
  if (id === "chat")
    void task("读取会话", async () => {
      await loadConversations();
      await loadCharacterOptions();
    });
}
function readHash() {
  const id = location.hash.slice(1);
  if (navigation.some((item) => item.id === id)) {
    if (
      id !== page.value &&
      characterCards.value?.hasUnsaved &&
      !window.confirm("角色卡有未保存的修改，确定离开？")
    ) {
      location.hash = page.value;
      return;
    }
    page.value = id as Page;
  }
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
    characterCards.value?.hasUnsaved &&
    !window.confirm("角色卡有未保存的修改，确定退出登录？")
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
async function loadCharacterOptions() {
  characterOptions.value = await api<CharacterCardList>("/character-cards");
}
function newConversationCharacter() {
  if (!characterChoice.value) return undefined;
  const selected = characterOptions.value?.items.find(
    (item) => item.id === characterChoice.value,
  );
  return selected?.latest_revision
    ? { card_id: selected.id, revision: selected.latest_revision }
    : undefined;
}
async function openTestConversation(conversation: Conversation) {
  await task("读取测试会话", async () => {
    await loadConversations();
    await loadCharacterOptions();
    messages.value = (
      await api<{ items: ChatMessage[] }>(
        `/conversations/${encodeURIComponent(conversation.id)}/messages`,
      )
    ).items;
    selectedConversation.value = conversation.id;
    chatRun.value = null;
    page.value = "chat";
    location.hash = "chat";
    mobileMenu.value = false;
    notice.value =
      "独立草稿测试会话。普通消息会请求模型，不会投递 QQ 或自动写入助手长期记忆。";
  });
}
async function createConversation() {
  await task("新建会话", async () => {
    const conversation = await api<Conversation>("/conversations", {
      method: "POST",
      body: {
        title: `新对话 ${conversations.value.length + 1}`,
        character: newConversationCharacter(),
      },
    });
    await loadConversations();
    selectedConversation.value = conversation.id;
    messages.value = [];
    chatRun.value = null;
  });
}
async function selectConversation(id: string) {
  if (chatRunning.value) return;
  await task("读取消息", async () => {
    const result = await api<{ items: ChatMessage[] }>(
      `/conversations/${encodeURIComponent(id)}/messages`,
    );
    selectedConversation.value = id;
    messages.value = result.items;
    chatRun.value = null;
    await scrollMessages();
  });
}
async function scrollMessages() {
  await nextTick();
  messageList.value?.scrollTo({
    top: messageList.value.scrollHeight,
    behavior: "smooth",
  });
}
async function sendMessage() {
  if (!chatInput.value.trim() || chatRunning.value || busy.value) return;
  const content = chatInput.value.trim();
  chatRunning.value = true;
  error.value = "";
  notice.value = "";
  try {
    if (!selectedConversation.value) {
      const conversation = await api<Conversation>("/conversations", {
        method: "POST",
        body: {
          title: content.slice(0, 28),
          character: newConversationCharacter(),
        },
      });
      selectedConversation.value = conversation.id;
      await loadConversations();
    }
    const id = selectedConversation.value!;
    const submitted = await api<{ run_id: string }>(
      `/conversations/${encodeURIComponent(id)}/messages`,
      {
        method: "POST",
        body: { content, client_message_id: crypto.randomUUID() },
      },
    );
    chatInput.value = "";
    messages.value = (
      await api<{ items: ChatMessage[] }>(
        `/conversations/${encodeURIComponent(id)}/messages`,
      )
    ).items;
    await scrollMessages();
    for (let index = 0; index < 360 && !disposed; index++) {
      chatRun.value = await api<Run>(
        `/runs/${encodeURIComponent(submitted.run_id)}`,
      );
      if (
        [
          "completed",
          "failed",
          "cancelled",
          "interrupted",
          "outcome_unknown",
        ].includes(chatRun.value.status)
      )
        break;
      await new Promise((resolve) => setTimeout(resolve, 1000));
    }
    messages.value = (
      await api<{ items: ChatMessage[] }>(
        `/conversations/${encodeURIComponent(id)}/messages`,
      )
    ).items;
    if (
      ["failed", "interrupted", "outcome_unknown"].includes(
        chatRun.value?.status || "",
      )
    )
      error.value =
        chatRun.value?.error || "模型调用未完成，请检查模型配置和服务日志。";
    if (
      chatRun.value &&
      ![
        "completed",
        "failed",
        "cancelled",
        "interrupted",
        "outcome_unknown",
      ].includes(chatRun.value.status)
    )
      notice.value = "任务仍在后台处理，可以稍后重新打开会话查看结果。";
    await scrollMessages();
    await refreshStatus();
  } catch (caught) {
    error.value = errorMessage(caught);
  } finally {
    chatRunning.value = false;
  }
}
function chatKeydown(event: KeyboardEvent) {
  if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    void sendMessage();
  }
}

onMounted(() => {
  readHash();
  window.addEventListener("hashchange", readHash);
  void initialize().then(() => {
    if (authState.value === "ready" && page.value === "chat")
      void task("读取角色", loadCharacterOptions);
  });
});
onUnmounted(() => {
  disposed = true;
  if (statusTimer !== undefined) clearTimeout(statusTimer);
  statusController?.abort();
  window.removeEventListener("hashchange", readHash);
  clearCsrfToken();
});
</script>

<template>
  <div v-if="authState !== 'ready'" class="auth-shell">
    <section class="auth-story">
      <a href="/admin/" class="brand auth-brand"
        ><img class="brand-mark" :src="brandMarkUrl" alt="DSH 鲸鱼标志" />
        <strong>DSH<span>助手控制台</span></strong></a
      >
      <div class="story-content">
        <span class="eyebrow light">A QUIET COMPANION, ALWAYS NEAR.</span>
        <h1>一方深海，<br />一位贴心助手。</h1>
        <p>把日常交给熟悉的陪伴，<br />在自己的设备上，安放心中的小小世界。</p>
        <div class="story-portrait">
          <span class="portrait-halo" aria-hidden="true"></span>
          <Icon class="portrait-spark" name="sparkles" :size="30" />
          <img
            class="mascot-image"
            :src="mascotUrl"
            alt="蓝发蓝眼、身着白色蕾丝与深蓝裙装的鲸鱼主题 DSH 助手"
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
        <h2>正在连接你的助手</h2>
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
              ? "完成首次设置，开始配置你的 AI 助手。"
              : "登录控制台，继续管理你的助手。"
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
    <aside class="sidebar" :class="{ open: mobileMenu }">
      <a href="#overview" class="brand" @click.prevent="navigate('overview')"
        ><img class="brand-mark" :src="brandMarkUrl" alt="DSH 鲸鱼标志" />
        <strong>DSH<span>助手控制台</span></strong></a
      >
      <div class="workspace-label">
        个人工作空间 <span class="tiny-badge">M1</span>
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
      <div class="sidebar-future">
        <span class="eyebrow">COMING NEXT</span>
        <p>微信 · 多模态 · 插件工厂</p>
        <span>后续阶段逐步开放</span>
      </div>
      <div class="companion-note">
        <Icon name="whale" :size="26" />
        <span>让每一次对话，<br />都有温柔的回响。</span>
        <Icon class="companion-bow" name="bow" :size="17" />
      </div>
      <div class="device-card">
        <div class="device-icon"><Icon name="cpu" /></div>
        <div>
          <strong>Raspberry Pi 5</strong><span>8GB · Lite 64 位部署基线</span>
        </div>
        <span class="status-dot"></span>
      </div>
      <div class="sidebar-user">
        <div class="avatar">
          {{ user?.username?.slice(0, 1).toUpperCase() }}
        </div>
        <div>
          <strong>{{ user?.username }}</strong
          ><span>{{ user?.role === "admin" ? "实例管理员" : user?.role }}</span>
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
            v-if="page !== 'chat' && page !== 'characters' && configState"
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
                    ? `${config.assistant.name || "DSH"} 的模型已配置，随时开启新的对话。`
                    : "连接一个模型，让你的专属助手从这里启航。"
                }}
              </p>
              <button
                class="button white"
                @click="navigate(configuredModel ? 'chat' : 'assistant')"
              >
                {{ configuredModel ? "开始一段对话" : "配置第一个模型"
                }}<Icon name="arrow" :size="17" />
              </button>
            </div>
            <div class="welcome-art">
              <span class="welcome-halo" aria-hidden="true"></span>
              <Icon class="welcome-spark" name="sparkles" :size="26" />
              <img
                class="mascot-image"
                :src="mascotUrl"
                alt="DSH Q 版助手"
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
                  <h2>让助手开始工作</h2>
                  <p>完成这几步，建立第一个工作闭环。</p>
                </div>
                <span class="progress-pill">{{ setupProgress }} / 3</span>
              </div>
              <div class="setup-list">
                <button @click="navigate('assistant')">
                  <span class="step-number" :class="{ done: configuredModel }"
                    ><Icon
                      v-if="configuredModel"
                      name="check"
                      :size="16"
                    /><template v-else>01</template></span
                  ><span
                    ><strong>连接 AI 模型</strong
                    ><small>填写模型地址、名称与 API Key</small></span
                  ><Icon name="chevron" :size="18" /></button
                ><button @click="navigate('chat')">
                  <span
                    class="step-number"
                    :class="{ done: conversations.length > 0 }"
                    ><Icon
                      v-if="conversations.length"
                      name="check"
                      :size="16"
                    /><template v-else>02</template></span
                  ><span
                    ><strong>发起一段对话</strong
                    ><small>验证配置，查看真实模型回复</small></span
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
                    ><strong>接入 QQ 机器人</strong
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
                当前为 M1 基础版。记忆、多模态与插件工厂将在后续阶段开放。
              </p>
            </section>
          </div>
        </template>

        <template v-if="page === 'assistant' || page === 'qq'">
          <div class="settings-layout">
            <div class="settings-main">
              <section v-if="page === 'assistant'" class="panel">
                <div class="panel-heading">
                  <div>
                    <h2>助手身份</h2>
                    <p>
                      这里的名称与提示词用于未绑定角色卡的对话。角色人格请在“角色卡”中配置。
                    </p>
                  </div>
                  <span class="soft-icon"><Icon name="settings" /></span>
                </div>
                <label class="field"
                  ><span>助手名称</span
                  ><input
                    v-model="config.assistant.name"
                    maxlength="100"
                    placeholder="例如：我的日常助手" /></label
                ><label class="field"
                  ><span>系统提示词</span
                  ><textarea
                    v-model="config.assistant.system_prompt"
                    rows="5"
                    maxlength="16000"
                    placeholder="描述助手的职责、语气与行为要求…"
                  ></textarea
                  ><small>提示词不会授予额外权限。</small></label
                >
                <div class="field-row">
                  <label class="field"
                    ><span>最大活跃任务数</span
                    ><select
                      v-model.number="config.assistant.max_concurrent_runs"
                    >
                      <option :value="1">1 个任务</option>
                      <option :value="2">2 个任务</option></select
                    ><small>为树莓派 8GB 内存保留余量。</small></label
                  ><label class="field"
                    ><span>任务超时（秒）</span
                    ><input
                      v-model.number="config.assistant.timeout_seconds"
                      type="number"
                      min="5"
                      max="300"
                  /></label>
                </div>
              </section>
              <section v-if="page === 'assistant'" class="panel">
                <div class="panel-heading">
                  <div>
                    <h2>模型服务</h2>
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
              </section>
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
                  ><select v-model="config.qq.transport">
                    <option value="websocket">
                      WebSocket · 主动连接，无需公网回调
                    </option>
                    <option value="webhook">
                      Webhook · HTTPS 回调
                    </option></select
                  ><small>QQ 开放平台的接入方式需与此处保持一致。</small></label
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

        <template v-if="page === 'history'">
          <section class="panel history-panel">
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
              <div class="revision-icon"><Icon name="clock" :size="19" /></div>
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
          </section>
        </template>

        <CharacterCards
          v-if="page === 'characters'"
          ref="characterCards"
          @changed="task('读取角色', loadCharacterOptions)"
          @test-conversation="openTestConversation"
          @auth-expired="expireSession"
        />

        <template v-if="page === 'chat'">
          <section class="chat-workspace">
            <aside class="conversation-sidebar">
              <label class="chat-character-choice"
                >新对话角色
                <select
                  v-model="characterChoice"
                  :disabled="chatRunning || !!busy"
                  aria-label="新对话角色"
                >
                  <option value="">
                    {{
                      characterOptions?.default
                        ? `默认：${characterOptions.default.name} v${characterOptions.default.revision}`
                        : "默认：原有助手"
                    }}
                  </option>
                  <option
                    v-for="card in characterOptions?.items.filter(
                      (item) => !item.archived_at && item.latest_revision,
                    )"
                    :key="card.id"
                    :value="card.id"
                  >
                    {{ card.name }} v{{ card.latest_revision }}
                  </option>
                </select>
                <small>仅影响新建对话；换角色请新建会话。</small>
              </label>
              <button
                class="button primary full"
                :disabled="!!busy || chatRunning"
                @click="createConversation"
              >
                <Icon name="plus" :size="17" />新建对话
              </button>
              <div class="conversation-label">
                最近对话 <span>{{ conversations.length }}</span>
              </div>
              <button
                v-for="conversation in conversations"
                :key="conversation.id"
                class="conversation-button"
                :class="{ active: selectedConversation === conversation.id }"
                :disabled="chatRunning"
                @click="selectConversation(conversation.id)"
              >
                <Icon name="chat" :size="17" /><span
                  ><strong>{{ conversation.title || "未命名对话" }}</strong
                  ><small>{{
                    formatDate(conversation.created_at)
                  }}</small></span
                >
              </button>
              <p v-if="!conversations.length" class="conversation-empty">
                你的对话会保存在设备上。<br />从第一条消息开始。
              </p>
            </aside>
            <div class="chat-main">
              <div class="chat-heading">
                <div class="assistant-avatar">
                  <img :src="brandMarkUrl" alt="" width="33" height="33" />
                </div>
                <div>
                  <strong>{{ conversationAssistantName }}</strong
                  ><span
                    >{{ conversationCharacterLabel }} ·
                    {{ status?.model?.model || config.model.model }}</span
                  >
                </div>
                <span
                  class="badge"
                  :class="configuredModel ? 'green' : 'neutral'"
                  >{{ configuredModel ? "文本模型" : "需要模型凭证" }}</span
                >
              </div>
              <div ref="messageList" class="message-list" aria-live="polite">
                <div v-if="!messages.length" class="chat-welcome">
                  <img
                    class="mascot-image chat-mascot"
                    :src="mascotUrl"
                    alt="等待与你对话的 DSH 助手"
                    width="128"
                    height="144"
                  />
                  <h2>开始一段新的对话</h2>
                  <template
                    v-if="
                      selectedConversationData?.greeting ||
                      selectedConversationData?.character?.greeting
                    "
                  >
                    <p class="character-greeting">
                      {{
                        selectedConversationData.greeting ||
                        selectedConversationData.character?.greeting
                      }}
                    </p>
                    <small class="muted">角色开场白 · 尚未生成模型回复</small>
                  </template>
                  <p v-else>
                    今天有什么想和我分享？<br />回复将使用你配置的模型。
                  </p>
                  <button
                    v-if="!configuredModel"
                    class="button secondary"
                    @click="navigate('assistant')"
                  >
                    配置模型<Icon name="arrow" :size="16" />
                  </button>
                </div>
                <article
                  v-for="message in messages"
                  :key="message.id"
                  class="message"
                  :class="
                    message.role === 'user' ? 'from-user' : 'from-assistant'
                  "
                >
                  <div class="message-author">
                    {{
                      message.role === "user"
                        ? "你"
                        : message.role === "assistant"
                          ? conversationAssistantName
                          : message.role
                    }}
                  </div>
                  <div class="message-content">{{ message.content }}</div>
                  <time>{{ formatDate(message.created_at) }}</time>
                </article>
                <div v-if="chatRunning" class="chat-pending">
                  <span class="typing-dots"><i></i><i></i><i></i></span
                  >{{
                    chatRun?.status === "queued"
                      ? "任务排队中…"
                      : "助手正在处理…"
                  }}
                </div>
              </div>
              <form class="chat-composer" @submit.prevent="sendMessage">
                <textarea
                  v-model="chatInput"
                  rows="2"
                  maxlength="16000"
                  placeholder="给助手发条消息…"
                  aria-label="对话消息"
                  :disabled="chatRunning"
                  @keydown="chatKeydown"
                ></textarea>
                <div>
                  <span>Enter 发送 · Shift + Enter 换行</span
                  ><button
                    class="button primary small"
                    type="submit"
                    :disabled="
                      chatRunning ||
                      !!busy ||
                      !chatInput.trim() ||
                      (!configuredModel && !memoryCommand)
                    "
                  >
                    <span v-if="chatRunning" class="spinner"></span
                    >{{ chatRunning ? "处理中" : "发送"
                    }}<Icon v-if="!chatRunning" name="arrow" :size="17" />
                  </button>
                </div>
              </form>
            </div>
          </section>
        </template>
        <div v-if="page === 'memory'" class="memory-page">
          <MemoryPanel />
          <ConversationSummary />
        </div>
        <footer class="page-footer">
          <span>DSH · 你的助手，你来掌控</span
          ><span title="前端构建提交"
            >Web {{ frontendBuild.slice(0, 10) }} · Rust 服务</span
          >
        </footer>
      </main>
    </div>
  </div>
</template>
