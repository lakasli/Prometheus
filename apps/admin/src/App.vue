<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { api, ApiError, setCsrf } from "./api";
import type {
  Capability,
  ConfigState,
  Log,
  Personality,
  PersonalityState,
  RecordItem,
  RecordKind,
  Status,
} from "./types";

type Page =
  | "overview"
  | "personality"
  | "models"
  | "channels"
  | RecordKind
  | "plugins"
  | "logs";
const pages: {
  id: Page;
  title: string;
  mark: string;
  group: string;
  intro: string;
}[] = [
  {
    id: "overview",
    title: "运行总览",
    mark: "◈",
    group: "日常空间",
    intro: "留意当下，也让每一步都有记录。",
  },
  {
    id: "personality",
    title: "性格",
    mark: "✳",
    group: "日常空间",
    intro: "性情、表达，以及彼此相处的约定。",
  },
  {
    id: "memory",
    title: "记忆与资料",
    mark: "◇",
    group: "日常空间",
    intro: "记得有来由的事，也尊重每一次更正与遗忘。",
  },
  {
    id: "diary",
    title: "日记",
    mark: "▤",
    group: "日常空间",
    intro: "把事实和感受分开，留下有来源的日常。",
  },
  {
    id: "schedule",
    title: "日程",
    mark: "◷",
    group: "日常空间",
    intro: "安排与进展，各自清楚。当前由你登记与更新。",
  },
  {
    id: "people",
    title: "人物",
    mark: "♧",
    group: "日常空间",
    intro: "每个人都有自己的名字与来历。",
  },
  {
    id: "relationships",
    title: "人际关系",
    mark: "⌘",
    group: "日常空间",
    intro: "以审核过的资料，连接具体的人。",
  },
  {
    id: "skills",
    title: "Skill 管理",
    mark: "ϟ",
    group: "连接与配置",
    intro: "保存做事的方法，每次对话显式选用。",
  },
  {
    id: "models",
    title: "模型与运行",
    mark: "⚙",
    group: "连接与配置",
    intro: "配置远程模型，查看连接结果与生效版本。",
  },
  {
    id: "channels",
    title: "渠道与媒体",
    mark: "◎",
    group: "连接与配置",
    intro: "各项连接的真实状态，在这里核对。",
  },
  {
    id: "plugins",
    title: "插件",
    mark: "⬡",
    group: "连接与配置",
    intro: "权限、隔离与执行，均以实际验证为准。",
  },
  {
    id: "logs",
    title: "日志记录",
    mark: "≡",
    group: "连接与配置",
    intro: "从接收、生成到保存，查看每一步的结果。",
  },
];
const page = ref<Page>("overview"),
  menuOpen = ref(false);
const active = computed(() => pages.find((item) => item.id === page.value)!);
const authState = ref<"loading" | "setup" | "login" | "ready" | "unavailable">(
  "loading",
);
const username = ref(""),
  authForm = reactive({
    username: "admin",
    password: "",
    confirmation: "",
    bootstrap_token: "",
  });
const error = ref(""),
  notice = ref(""),
  busy = ref(false),
  status = ref<Status | null>(null),
  loadedAt = ref<Date | null>(null);
const configState = ref<ConfigState | null>(null),
  personalityState = ref<PersonalityState | null>(null);
const personality = reactive<Personality>({
  name: "",
  description: "",
  personality: "",
  scenario: "",
  system_prompt: "",
  mes_example: "",
});
const config = ref<ConfigState["config"] | null>(null),
  secrets = ref<{ id: string; kind: string; configured: boolean }[]>([]),
  newKey = ref("");
const revisions = ref<{ revision: number; created_at: number }[]>([]);
const capability = ref<Record<string, Capability>>({});
const records = ref<RecordItem[]>([]),
  people = ref<RecordItem[]>([]),
  sources = ref<RecordItem[]>([]),
  total = ref(0),
  offset = ref(0),
  query = ref("");
const logs = ref<Log[]>([]),
  nextLog = ref<number | null>(null),
  runDetail = ref<Record<string, unknown> | null>(null);
const editing = ref<RecordItem | null>(null),
  editor = ref(false),
  publish = ref(false),
  visibility = ref<"private" | "shared">("private");
const form = reactive<Record<string, any>>({}),
  startTime = ref(""),
  endTime = ref("");
const controller = new AbortController();
const clone = <T,>(value: T): T => JSON.parse(JSON.stringify(value)) as T;
const baseline = ref("");
const dirty = computed(
  () =>
    editor.value ||
    (page.value === "personality" &&
      baseline.value !== JSON.stringify(personality)) ||
    (page.value === "models" &&
      (baseline.value !== JSON.stringify(config.value) || !!newKey.value)),
);
function beforeUnload(event: BeforeUnloadEvent) {
  if (authState.value === "ready" && dirty.value) event.preventDefault();
}
async function refresh() {
  if (
    dirty.value &&
    !window.confirm("重新载入会放弃尚未保存的修改，确定继续？")
  )
    return;
  await task(loadPage);
}
let disposed = false,
  epoch = 0;
const frontendBuild = import.meta.env.VITE_BUILD_SHA || "development";
const brandUrl = `${import.meta.env.BASE_URL}flame.svg`;
const isRecords = computed(() =>
  ["memory", "diary", "schedule", "people", "relationships", "skills"].includes(
    page.value,
  ),
);
const labels: Record<string, string> = {
  completed: "已完成",
  failed: "失败",
  cancelled: "已取消",
  interrupted: "已中断",
  queued: "排队中",
  running: "运行中",
  planned: "已安排",
  in_progress: "进行中",
  authentication: "登录",
  config: "配置",
  personality: "性格",
  secret: "凭据",
  run: "对话",
  memory: "记忆",
  diary: "日记",
  schedule: "日程",
  people: "人物",
  relationships: "关系",
  skills: "Skill",
};
const date = (value: number) =>
  new Date(value * 1000).toLocaleString("zh-CN", {
    timeZone: "Asia/Shanghai",
    hour12: false,
  });
const bytes = (value: number) => `${(value / 1024 ** 3).toFixed(1)} GB`;
const localDate = () =>
  new Intl.DateTimeFormat("en-CA", { timeZone: "Asia/Shanghai" }).format(
    new Date(),
  );
const timeInput = (value: number) => {
  const d = new Date(value * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
};
const personName = (id: string) =>
  people.value.find((item) => item.id === id)?.data.name || "已失效的人物";
function failure(value: unknown) {
  if (disposed) return;
  if (value instanceof ApiError && value.status === 401) {
    authState.value = "login";
    setCsrf("");
    newKey.value = "";
  }
  error.value = value instanceof Error ? value.message : "连接未完成，请重试";
}
async function task(action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  notice.value = "";
  try {
    await action();
  } catch (value) {
    failure(value);
  } finally {
    busy.value = false;
  }
}
async function initialize() {
  error.value = "";
  try {
    const state = await api<{ initialized: boolean }>(
      "/auth/status",
      "GET",
      undefined,
      undefined,
      controller.signal,
    );
    if (!state.initialized) {
      authState.value = "setup";
      return;
    }
    const me = await api<{ user: { username: string }; csrf_token: string }>(
      "/auth/me",
      "GET",
      undefined,
      undefined,
      controller.signal,
    );
    username.value = me.user.username;
    setCsrf(me.csrf_token);
    authState.value = "ready";
    await loadPage();
  } catch (value) {
    if (value instanceof ApiError && value.status === 401)
      authState.value = "login";
    else {
      authState.value = "unavailable";
      failure(value);
    }
  }
}
async function login() {
  await task(async () => {
    if (
      authState.value === "setup" &&
      authForm.password !== authForm.confirmation
    )
      throw new Error("两次密码输入不一致");
    const data = await api<{ user: { username: string }; csrf_token: string }>(
      authState.value === "setup" ? "/auth/bootstrap" : "/auth/login",
      "POST",
      authState.value === "setup"
        ? {
            username: authForm.username,
            password: authForm.password,
            bootstrap_token: authForm.bootstrap_token,
          }
        : { username: authForm.username, password: authForm.password },
    );
    authForm.password = "";
    authForm.confirmation = "";
    authForm.bootstrap_token = "";
    username.value = data.user.username;
    setCsrf(data.csrf_token);
    authState.value = "ready";
    await loadPage();
  });
}
async function logout() {
  await task(async () => {
    await api("/auth/logout", "POST", {});
    setCsrf("");
    newKey.value = "";
    records.value = [];
    logs.value = [];
    runDetail.value = null;
    authState.value = "login";
  });
}
async function navigate(next: Page) {
  if (busy.value || next === page.value) return;
  if (dirty.value && !window.confirm("离开会放弃尚未保存的修改，确定继续？"))
    return;
  editor.value = false;
  page.value = next;
  menuOpen.value = false;
  error.value = "";
  notice.value = "";
  offset.value = 0;
  query.value = "";
  runDetail.value = null;
  await task(loadPage);
}
async function loadPage() {
  const version = ++epoch;
  if (page.value === "overview") {
    const [state, capabilities, events] = await Promise.all([
      api<Status>("/status"),
      api<{ capabilities: Record<string, Capability> }>("/capabilities"),
      api<{ items: Log[]; next_cursor: number | null }>("/runtime-logs"),
    ]);
    if (version !== epoch || disposed) return;
    status.value = state;
    capability.value = capabilities.capabilities;
    logs.value = events.items.slice(0, 5);
    loadedAt.value = new Date();
  } else if (page.value === "personality") {
    const data = await api<PersonalityState>("/assistant/personality");
    personalityState.value = data;
    Object.assign(personality, data.personality);
    baseline.value = JSON.stringify(personality);
  } else if (page.value === "models") {
    const [state, keys, history] = await Promise.all([
      api<ConfigState>("/config"),
      api<{ items: typeof secrets.value }>("/secrets"),
      api<{ items: typeof revisions.value }>("/config/revisions"),
    ]);
    configState.value = state;
    config.value = clone(state.config);
    secrets.value = keys.items;
    revisions.value = history.items;
    baseline.value = JSON.stringify(config.value);
  } else if (page.value === "channels" || page.value === "plugins") {
    capability.value = (
      await api<{ capabilities: Record<string, Capability> }>("/capabilities")
    ).capabilities;
  } else if (page.value === "logs") {
    const result = await api<{ items: Log[]; next_cursor: number | null }>(
      "/runtime-logs",
    );
    logs.value = result.items;
    nextLog.value = result.next_cursor;
  } else if (isRecords.value) {
    const params = new URLSearchParams({
      q: query.value,
      offset: String(offset.value),
      limit: "30",
    });
    const result = await api<{ items: RecordItem[]; total: number }>(
      `/records/${page.value}?${params}`,
    );
    if (version !== epoch || disposed) return;
    records.value = result.items;
    total.value = result.total;
    if (page.value === "relationships")
      people.value = (
        await api<{ items: RecordItem[] }>("/records/people?limit=100")
      ).items;
    if (page.value === "diary")
      sources.value = (
        await api<{ items: RecordItem[] }>("/records/memory?limit=100")
      ).items;
  }
}
function openEditor(item?: RecordItem) {
  editing.value = item ?? null;
  visibility.value = item?.visibility ?? "private";
  publish.value = false;
  for (const key of Object.keys(form)) delete form[key];
  const defaults: Record<RecordKind, Record<string, any>> = {
    memory: { content: "" },
    diary: {
      title: "",
      content: "",
      feelings: "",
      date: localDate(),
      period: "day",
      sources: [],
    },
    schedule: { title: "", content: "", status: "planned" },
    people: { name: "", notes: "" },
    relationships: { from_id: "", to_id: "", description: "" },
    skills: { name: "", description: "", instructions: "", enabled: true },
  };
  Object.assign(form, clone(defaults[page.value as RecordKind]));
  if (item)
    for (const key of Object.keys(form))
      form[key] = clone(item.data[key] ?? form[key]);
  startTime.value = timeInput(
    item ? Number(item.data.start_at) : Date.now() / 1000,
  );
  endTime.value = timeInput(
    item ? Number(item.data.end_at) : Date.now() / 1000 + 3600,
  );
  editor.value = true;
}
async function saveRecord() {
  await task(async () => {
    const data = clone(form);
    if (page.value === "schedule") {
      data.start_at = Math.floor(new Date(startTime.value).getTime() / 1000);
      data.end_at = Math.floor(new Date(endTime.value).getTime() / 1000);
    }
    const path = `/records/${page.value}${editing.value ? `/${editing.value.id}` : ""}`;
    await api(
      path,
      editing.value ? "PUT" : "POST",
      { data, visibility: visibility.value, publish: publish.value },
      editing.value?.revision,
    );
    editor.value = false;
    await loadPage();
    notice.value = "已保存。";
  });
}
async function forget(item: RecordItem) {
  if (
    !window.confirm(
      "确定遗忘这条资料？其派生日记或关系也会失效。原始对话与离线备份保留。",
    )
  )
    return;
  await task(async () => {
    await api(
      `/records/${item.kind}/${item.id}`,
      "DELETE",
      undefined,
      item.revision,
    );
    await loadPage();
    notice.value = "资料已遗忘，依赖资料已失效。";
  });
}
async function savePersonality() {
  await task(async () => {
    const saved = await api<PersonalityState>(
      "/assistant/personality",
      "PUT",
      { personality },
      personalityState.value!.revision,
    );
    personalityState.value = saved;
    baseline.value = JSON.stringify(personality);
    notice.value = "性格已保存并生效，用于下一次运行。";
  });
}
async function saveModel() {
  await task(async () => {
    const saved = await api<ConfigState>(
      "/config",
      "PUT",
      { config: config.value },
      configState.value!.revision,
    );
    configState.value = saved;
    await loadPage();
    notice.value = "配置已保存并生效。";
  });
}
async function storeKey() {
  await task(async () => {
    const saved = await api<{ id: string }>("/secrets", "POST", {
      kind: "model_api_key",
      value: newKey.value,
    });
    newKey.value = "";
    secrets.value = (
      await api<{ items: typeof secrets.value }>("/secrets")
    ).items;
    config.value!.model.credential_ref = saved.id;
    notice.value = "凭据已加密保存。请检查连接并保存模型配置。";
  });
}
async function testModel() {
  await task(async () => {
    const result = await api<{ message: string }>("/config/check", "POST", {
      config: config.value,
    });
    notice.value = result.message;
  });
}
async function rollback(rev: number) {
  if (
    !window.confirm(
      `将配置回退到版本 ${rev} 的内容，并保存为一个新版本，确定继续？`,
    )
  )
    return;
  await task(async () => {
    await api(
      `/config/revisions/${rev}/rollback`,
      "POST",
      {},
      configState.value!.revision,
    );
    await loadPage();
    notice.value = "配置已回退并以新版本生效。";
  });
}
async function loadMoreLogs() {
  if (!nextLog.value) return;
  await task(async () => {
    const result = await api<{ items: Log[]; next_cursor: number | null }>(
      `/runtime-logs?cursor=${nextLog.value}`,
    );
    logs.value.push(...result.items);
    nextLog.value = result.next_cursor;
  });
}
async function inspect(log: Log) {
  if (log.kind === "run" && log.resource_id)
    await task(async () => {
      runDetail.value = await api(`/runs/${log.resource_id}`);
    });
}
async function exportData() {
  await task(async () => {
    const data = await api("/export");
    const url = URL.createObjectURL(
      new Blob([JSON.stringify(data, null, 2)], { type: "application/json" }),
    );
    const link = document.createElement("a");
    link.href = url;
    link.download = `prometheus-${localDate()}.json`;
    link.click();
    URL.revokeObjectURL(url);
    notice.value = "已导出当前资料与配置，凭据不包含在内。";
  });
}
onMounted(() => {
  void initialize();
  window.addEventListener("beforeunload", beforeUnload);
});
onUnmounted(() => {
  disposed = true;
  controller.abort();
  window.removeEventListener("beforeunload", beforeUnload);
});
</script>

<template>
  <main v-if="authState !== 'ready'" class="auth-layout">
    <section class="auth-story">
      <a class="brand" href="/admin/"
        ><img :src="brandUrl" alt="" /><span
          >Prometheus<small>日常空间</small></span
        ></a
      >
      <div>
        <p class="eyebrow">A LITTLE LIGHT, EVERY DAY</p>
        <h1>把日子里的微光，<br />慢慢留住。</h1>
        <p>有来由的记忆，有分寸的陪伴。<br />从今天的一次相遇开始。</p>
      </div>
      <span class="story-footer">Prometheus · 让日常有迹可循</span>
    </section>
    <section class="auth-form">
      <div class="auth-card">
        <p class="eyebrow">WELCOME HOME</p>
        <h2>
          {{ authState === "setup" ? "建立你的日常空间" : "很高兴，又见面了" }}
        </h2>
        <p class="muted">
          {{
            authState === "setup"
              ? "先创建管理账户。没有默认密码。"
              : "登录后，继续照看这里的日常。"
          }}
        </p>
        <p v-if="error" class="alert error" role="alert">{{ error }}</p>
        <p v-if="authState === 'loading'" class="muted" role="status">
          正在连接本机服务…
        </p>
        <div v-else-if="authState === 'unavailable'">
          <p>请确认 Prometheus 已在本机启动。</p>
          <button @click="initialize">重新连接</button>
        </div>
        <form v-else @submit.prevent="login">
          <label
            >用户名<input
              v-model="authForm.username"
              autocomplete="username"
              required
              minlength="3"
              maxlength="64" /></label
          ><label
            >密码<input
              v-model="authForm.password"
              type="password"
              :autocomplete="
                authState === 'setup' ? 'new-password' : 'current-password'
              "
              required
              :minlength="authState === 'setup' ? 12 : 1"
              maxlength="256" /></label
          ><template v-if="authState === 'setup'"
            ><label
              >确认密码<input
                v-model="authForm.confirmation"
                type="password"
                autocomplete="new-password"
                required
                minlength="12" /></label
            ><label
              >一次性初始化凭证<input
                v-model="authForm.bootstrap_token"
                type="password"
                autocomplete="off"
                required
              /><small
                >在本机运行 npm run
                bootstrap，从提示的私有文件读取。15分钟有效。</small
              ></label
            ></template
          ><button class="primary wide" :disabled="busy">
            {{
              busy
                ? "正在连接…"
                : authState === "setup"
                  ? "创建空间"
                  : "进入日常空间"
            }}
            <span>→</span>
          </button>
        </form>
        <p class="auth-note">此入口仅用于管理。凭据留在服务端。</p>
      </div>
    </section>
  </main>
  <div v-else class="workspace">
    <aside class="sidebar" :class="{ open: menuOpen }">
      <a class="brand" href="/admin/"
        ><img :src="brandUrl" alt="" /><span
          >Prometheus<small>日常空间</small></span
        ></a
      >
      <nav aria-label="主要导航">
        <template v-for="(item, index) in pages" :key="item.id"
          ><p
            v-if="index === 0 || pages[index - 1].group !== item.group"
            class="nav-group"
          >
            {{ item.group }}
          </p>
          <button
            :class="{ selected: page === item.id }"
            :aria-current="page === item.id ? 'page' : undefined"
            @click="navigate(item.id)"
          >
            <span class="nav-mark" aria-hidden="true">{{ item.mark }}</span
            >{{ item.title
            }}<span v-if="page === item.id" class="nav-dot"></span></button
        ></template>
      </nav>
      <footer class="sidebar-footer">
        <span class="avatar">{{ username.slice(0, 1).toUpperCase() }}</span>
        <div>
          <strong>{{ username }}</strong
          ><small>空间管理员</small>
        </div>
        <button
          class="icon-button"
          aria-label="退出登录"
          title="退出登录"
          @click="logout"
        >
          ↪
        </button>
      </footer>
    </aside>
    <div class="workspace-main">
      <header class="topbar">
        <div>
          <button
            class="menu-button"
            aria-label="展开导航"
            @click="menuOpen = !menuOpen"
          >
            ☰</button
          ><span
            >你的空间 <span class="crumb">/</span>
            <strong>{{ active.title }}</strong></span
          >
        </div>
        <span class="local-badge"><i></i> Windows 本机</span>
      </header>
      <main class="content">
        <div class="page-heading">
          <div>
            <p class="eyebrow">
              {{
                page === "overview"
                  ? "YOUR EVERYDAY, CONNECTED"
                  : "PROMETHEUS / " + page.toUpperCase()
              }}
            </p>
            <h1>{{ active.title }}</h1>
            <p class="muted">{{ active.intro }}</p>
          </div>
          <button class="secondary" :disabled="busy" @click="refresh">
            ↻ 刷新
          </button>
        </div>
        <p v-if="error" class="alert error" role="alert">{{ error }}</p>
        <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
        <p v-if="busy" class="loading" role="status">正在处理…</p>
        <template v-if="page === 'overview' && status">
          <section class="welcome-card">
            <div>
              <span class="pill">每一天，都值得认真相处</span>
              <h2>你好，{{ username }}。<br />今天也慢慢来。</h2>
              <p>让记忆有来源，让安排有进展。<br />这里是刘嘉卉的日常空间。</p>
              <button class="text-button" @click="navigate('personality')">
                看看她的性格 <span>↗</span>
              </button>
            </div>
            <div class="sun-art" aria-hidden="true">
              <div class="sun-disc"></div>
              <div class="sun-line one"></div>
              <div class="sun-line two"></div>
              <span>KEEP A LITTLE LIGHT</span>
            </div>
          </section>
          <section class="stats-grid">
            <article class="stat-card">
              <span class="stat-icon">◉</span>
              <p>运行状态</p>
              <h3>运行中 <span class="status-dot"></span></h3>
              <small
                >已连续运行
                {{ Math.floor(status.uptime_seconds / 60) }} 分钟</small
              >
            </article>
            <article class="stat-card">
              <span class="stat-icon">ϟ</span>
              <p>当前任务</p>
              <h3>
                {{ status.queue.running
                }}<small>运行 / {{ status.queue.queued }} 排队</small>
              </h3>
              <small>最多 2 个实际模型请求</small>
            </article>
            <article class="stat-card">
              <span class="stat-icon">◇</span>
              <p>可用内存</p>
              <h3>{{ bytes(status.platform.memory_available_bytes) }}</h3>
              <small>共 {{ bytes(status.platform.memory_total_bytes) }}</small>
            </article>
            <article class="stat-card">
              <span class="stat-icon">✓</span>
              <p>配置版本</p>
              <h3>v{{ status.config.applied_revision }}</h3>
              <small>已保存 · 已生效</small>
            </article>
          </section>
          <div class="overview-grid">
            <section class="panel">
              <div class="section-heading">
                <div>
                  <h2>最近的记录</h2>
                  <p>每一步，都有真实的结果。</p>
                </div>
                <button class="text-button" @click="navigate('logs')">
                  查看全部 ↗
                </button>
              </div>
              <div v-if="!logs.length" class="empty">
                还没有运行记录。一次真实的互动后，就会留在这里。
              </div>
              <ol v-else class="timeline">
                <li v-for="log in logs" :key="log.id">
                  <span class="timeline-dot"></span>
                  <div>
                    <strong>{{ log.title }}</strong
                    ><small>{{ date(log.created_at) }}</small>
                  </div>
                  <span class="badge">{{
                    labels[log.status] || log.status
                  }}</span>
                </li>
              </ol>
            </section>
            <section class="panel">
              <div class="section-heading">
                <div>
                  <h2>照看你的空间</h2>
                  <p>从一件小事开始。</p>
                </div>
              </div>
              <button class="quick-link" @click="navigate('models')">
                <span class="quick-icon">⚙</span
                ><span
                  ><strong>连接远程模型</strong
                  ><small>保存凭据，检查连接与生效状态</small></span
                ><b>→</b></button
              ><button class="quick-link" @click="navigate('memory')">
                <span class="quick-icon">◇</span
                ><span
                  ><strong>留下一条记忆</strong
                  ><small>默认私有，共享需要明确确认</small></span
                ><b>→</b></button
              ><button class="quick-link" @click="navigate('schedule')">
                <span class="quick-icon">◷</span
                ><span
                  ><strong>安排今天的日程</strong
                  ><small>记录计划，再更新真实进展</small></span
                ><b>→</b>
              </button>
            </section>
          </div>
          <div class="runtime-note">
            <span
              >Node.js {{ status.platform.node }} · {{ status.platform.arch }} ·
              {{ status.version }}</span
            ><span
              >采样于 {{ loadedAt?.toLocaleTimeString("zh-CN") }} ·
              {{
                status.commit === "development"
                  ? "开发版本"
                  : status.commit.slice(0, 12)
              }}</span
            >
          </div>
        </template>
        <section v-if="page === 'personality'" class="panel form-panel">
          <div class="section-heading">
            <div>
              <h2>刘嘉卉的性格</h2>
              <p>已保存 v{{ personalityState?.revision }} · 下一次运行生效</p>
            </div>
            <span class="badge">单一稳定身份</span>
          </div>
          <form @submit.prevent="savePersonality">
            <label
              >姓名<input
                v-model="personality.name"
                maxlength="128"
                required /></label
            ><label
              >个人简介<textarea
                v-model="personality.description"
                rows="3"
                maxlength="8000"
              ></textarea></label
            ><label
              >性情与脾气<textarea
                v-model="personality.personality"
                rows="5"
                maxlength="8000"
              ></textarea></label
            ><label
              >相处情境<textarea
                v-model="personality.scenario"
                rows="4"
                maxlength="8000"
              ></textarea></label
            ><label
              >相处约定<textarea
                v-model="personality.system_prompt"
                rows="8"
                maxlength="8000"
              ></textarea></label
            ><label
              >表达示例<textarea
                v-model="personality.mes_example"
                rows="5"
                maxlength="8000"
              ></textarea
              ><small
                >示例用于表达参考，不作为真实经历或人物事实。</small
              ></label
            ><button class="primary" :disabled="busy">保存并生效</button>
          </form>
        </section>
        <template v-if="isRecords"
          ><div class="record-toolbar">
            <form
              class="search-form"
              @submit.prevent="
                offset = 0;
                task(loadPage);
              "
            >
              <input
                v-model="query"
                aria-label="搜索资料"
                placeholder="搜索这里的资料…"
                maxlength="200"
              /><button class="secondary" :disabled="busy">搜索</button>
            </form>
            <button class="primary" :disabled="busy" @click="openEditor()">
              ＋ 新增{{ active.title }}
            </button>
          </div>
          <div v-if="!records.length && !busy" class="panel empty">
            <span class="empty-mark">{{ active.mark }}</span>
            <h2>这里，等你写下第一笔</h2>
            <p>点击右上角新增，将资料保存在你的空间。</p>
          </div>
          <section v-else class="record-grid">
            <article
              v-for="item in records"
              :key="item.id"
              class="panel record-card"
            >
              <div class="record-top">
                <span class="badge"
                  >{{ item.visibility === "private" ? "私有" : "共享" }} · v{{
                    item.revision
                  }}</span
                ><span v-if="item.kind === 'skills'" class="badge">{{
                  item.data.enabled ? "已启用" : "已停用"
                }}</span
                ><span v-if="item.kind === 'schedule'" class="badge">{{
                  labels[item.data.status]
                }}</span>
              </div>
              <h2 v-if="item.data.title || item.data.name">
                {{ item.data.title || item.data.name }}
              </h2>
              <h2 v-if="item.kind === 'relationships'">
                {{ personName(item.data.from_id) }} →
                {{ personName(item.data.to_id) }}
              </h2>
              <p v-if="item.kind === 'diary'" class="muted">
                {{ item.data.date }} ·
                {{
                  { day: "日记", week: "周记", month: "月记" }[
                    item.data.period as string
                  ]
                }}
              </p>
              <p v-if="item.kind === 'schedule'" class="muted">
                {{ date(item.data.start_at) }} — {{ date(item.data.end_at) }}
              </p>
              <p class="record-body">
                {{
                  item.data.content ||
                  item.data.notes ||
                  item.data.description ||
                  item.data.instructions
                }}
              </p>
              <p v-if="item.data.feelings" class="feeling">
                感受：{{ item.data.feelings }}
              </p>
              <small v-if="item.kind === 'diary'" class="muted"
                >人工记录 · {{ item.data.sources.length }} 条来源</small
              >
              <footer>
                <small>{{ date(item.updated_at) }}</small>
                <div>
                  <button class="text-button" @click="openEditor(item)">
                    编辑</button
                  ><button class="text-button danger" @click="forget(item)">
                    遗忘
                  </button>
                </div>
              </footer>
            </article>
          </section>
          <div class="pagination">
            <span>共 {{ total }} 条</span>
            <div>
              <button
                class="secondary"
                :disabled="offset === 0 || busy"
                @click="
                  offset -= 30;
                  task(loadPage);
                "
              >
                上一页</button
              ><button
                class="secondary"
                :disabled="offset + 30 >= total || busy"
                @click="
                  offset += 30;
                  task(loadPage);
                "
              >
                下一页
              </button>
            </div>
          </div></template
        >
        <template v-if="page === 'models' && config"
          ><section class="panel form-panel">
            <div class="section-heading">
              <div>
                <h2>远程模型</h2>
                <p>
                  已保存 v{{ configState?.revision }} · 已生效 v{{
                    configState?.applied_revision
                  }}
                </p>
              </div>
              <span class="badge">OpenAI 兼容协议</span>
            </div>
            <form @submit.prevent="saveModel">
              <label
                >服务地址<input
                  v-model="config.model.base_url"
                  type="url"
                  required
                  placeholder="https://api.deepseek.com"
                /><small
                  >使用 HTTPS。请求时校验实际解析地址，禁止受保护网络。</small
                ></label
              ><label
                >模型名称<input
                  v-model="config.model.model"
                  required
                  maxlength="128" /></label
              ><label
                >模型凭据<select v-model="config.model.credential_ref">
                  <option :value="null">尚未选择</option>
                  <option
                    v-for="key in secrets.filter(
                      (s) => s.kind === 'model_api_key',
                    )"
                    :key="key.id"
                    :value="key.id"
                  >
                    已配置 · {{ key.id.slice(0, 8) }}
                  </option>
                </select></label
              >
              <div class="credential-box">
                <label
                  >新增或替换凭据<input
                    v-model="newKey"
                    type="password"
                    autocomplete="off"
                    placeholder="只写入，不回显"
                    maxlength="8192" /></label
                ><button
                  type="button"
                  class="secondary"
                  :disabled="busy || !newKey.trim()"
                  @click="storeKey"
                >
                  加密保存凭据</button
                ><small>凭据独立加密保存，不随配置导出。</small>
              </div>
              <h3>运行参数</h3>
              <div class="form-row">
                <label
                  >并发上限<input
                    v-model.number="config.runtime.max_concurrent_runs"
                    type="number"
                    min="1"
                    max="2"
                    required /></label
                ><label
                  >请求超时（秒）<input
                    v-model.number="config.runtime.timeout_seconds"
                    type="number"
                    min="5"
                    max="120"
                    required /></label
                ><label
                  >历史消息数量<input
                    v-model.number="config.runtime.max_history_messages"
                    type="number"
                    min="2"
                    max="40"
                    required
                /></label>
              </div>
              <div class="form-actions">
                <button
                  type="button"
                  class="secondary"
                  :disabled="busy || !config.model.credential_ref"
                  @click="testModel"
                >
                  检查连接</button
                ><button class="primary" :disabled="busy">保存并生效</button>
              </div>
            </form>
          </section>
          <section class="panel history-panel">
            <div class="section-heading">
              <div>
                <h2>配置历史</h2>
                <p>回退将保存为新版本；不会恢复或删除数据。</p>
              </div>
              <button class="secondary" :disabled="busy" @click="exportData">
                导出资料
              </button>
            </div>
            <div
              v-for="rev in revisions"
              :key="rev.revision"
              class="history-row"
            >
              <strong>版本 {{ rev.revision }}</strong
              ><span class="muted">{{ date(rev.created_at) }}</span
              ><span v-if="rev.revision === configState?.revision" class="badge"
                >当前生效</span
              ><button
                v-else
                class="text-button"
                :disabled="busy"
                @click="rollback(rev.revision)"
              >
                回退到此内容
              </button>
            </div>
          </section></template
        >
        <template v-if="page === 'channels' || page === 'plugins'"
          ><p class="info-note">
            这是 Prometheus 独立重写版。下列能力逐项迁移与验收，历史 DSH
            测试结果不计为新版通过。
          </p>
          <section class="capability-grid">
            <article
              v-for="key in page === 'channels'
                ? ['qq', 'weixin', 'speech', 'vision', 'rest']
                : ['plugins', 'harness', 'native_text', 'skills']"
              :key="key"
              class="panel capability-card"
            >
              <div class="record-top">
                <span class="stat-icon">{{
                  page === "plugins" ? "⬡" : "◎"
                }}</span
                ><span
                  class="badge"
                  :class="{ available: capability[key]?.available }"
                  >{{
                    capability[key]?.available ? "已实现" : "待迁移 · 禁用"
                  }}</span
                >
              </div>
              <h2>
                {{
                  {
                    qq: "QQ 连接",
                    weixin: "微信连接",
                    speech: "语音与媒体",
                    vision: "图片理解",
                    rest: "睡眠与作息",
                    plugins: "生成插件",
                    harness: "Harness",
                    native_text: "原生文字对话",
                    skills: "文字 Skill",
                  }[key]
                }}
              </h2>
              <p class="muted">{{ capability[key]?.description }}</p>
            </article>
          </section></template
        >
        <section v-if="page === 'logs'" class="panel">
          <div class="section-heading">
            <div>
              <h2>运行记录</h2>
              <p>保留30天，最多10,000条。消息与运行按账户隔离。</p>
            </div>
          </div>
          <div v-if="!logs.length" class="empty">还没有日志记录。</div>
          <div v-for="log in logs" :key="log.id" class="log-row">
            <span class="log-icon">{{ log.kind === "run" ? "ϟ" : "·" }}</span>
            <div>
              <strong>{{ log.title }}</strong
              ><small
                >{{ date(log.created_at) }} ·
                {{ labels[log.kind] || log.kind }}</small
              >
            </div>
            <span class="badge">{{ labels[log.status] || log.status }}</span
            ><button
              v-if="log.kind === 'run'"
              class="text-button"
              :disabled="busy"
              @click="inspect(log)"
            >
              查看结果
            </button>
          </div>
          <button
            v-if="nextLog"
            class="secondary"
            :disabled="busy"
            @click="loadMoreLogs"
          >
            加载更多
          </button>
        </section>
        <footer class="content-footer">
          <span
            >Prometheus
            <span class="footer-dot">·</span> 把日常，慢慢留住。</span
          ><span>{{
            frontendBuild === "development"
              ? "开发版本"
              : frontendBuild.slice(0, 12)
          }}</span>
        </footer>
      </main>
    </div>
    <div
      v-if="editor"
      class="modal-backdrop"
      @click.self="!busy && (editor = false)"
    >
      <section
        class="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="editor-title"
      >
        <div class="section-heading">
          <h2 id="editor-title">
            {{ editing ? "编辑" : "新增" }}{{ active.title }}
          </h2>
          <button
            class="icon-button"
            aria-label="关闭编辑"
            :disabled="busy"
            @click="editor = false"
          >
            ×
          </button>
        </div>
        <p v-if="error" class="alert error" role="alert">{{ error }}</p>
        <form @submit.prevent="saveRecord">
          <label v-if="'name' in form"
            >名称<input v-model="form.name" required maxlength="128" /></label
          ><label v-if="'title' in form"
            >标题<input v-model="form.title" required maxlength="200"
          /></label>
          <div v-if="page === 'diary'" class="form-row">
            <label>日期<input v-model="form.date" type="date" required /></label
            ><label
              >周期<select v-model="form.period">
                <option value="day">日记</option>
                <option value="week">周记</option>
                <option value="month">月记</option>
              </select></label
            >
          </div>
          <label v-if="'content' in form"
            >{{ page === "diary" ? "事实与经历" : "内容"
            }}<textarea
              v-model="form.content"
              rows="5"
              :required="page !== 'schedule'"
              maxlength="16000"
            ></textarea></label
          ><label v-if="'feelings' in form"
            >感受<textarea
              v-model="form.feelings"
              rows="3"
              maxlength="4000"
            ></textarea></label
          ><label v-if="'notes' in form"
            >人物资料<textarea
              v-model="form.notes"
              rows="5"
              maxlength="8000"
            ></textarea></label
          ><template v-if="page === 'relationships'"
            ><label
              >起点人物<select v-model="form.from_id" required>
                <option value="">请选择</option>
                <option
                  v-for="person in people"
                  :key="person.id"
                  :value="person.id"
                >
                  {{ person.data.name }}
                </option>
              </select></label
            ><label
              >终点人物<select v-model="form.to_id" required>
                <option value="">请选择</option>
                <option
                  v-for="person in people"
                  :key="person.id"
                  :value="person.id"
                >
                  {{ person.data.name }}
                </option>
              </select></label
            ></template
          ><label v-if="'description' in form"
            >说明<textarea
              v-model="form.description"
              rows="3"
              :required="page === 'relationships'"
              maxlength="4000"
            ></textarea></label
          ><template v-if="page === 'schedule'"
            ><div class="form-row">
              <label
                >开始时间<input
                  v-model="startTime"
                  type="datetime-local"
                  required /></label
              ><label
                >结束时间<input
                  v-model="endTime"
                  type="datetime-local"
                  required
              /></label>
            </div>
            <small>按当前设备时区输入，列表统一显示上海时间。</small
            ><label
              >进度<select v-model="form.status">
                <option value="planned">已安排</option>
                <option value="in_progress">进行中</option>
                <option value="completed">已完成</option>
                <option value="cancelled">已取消</option>
              </select></label
            ></template
          ><template v-if="page === 'skills'"
            ><label
              >方法与步骤<textarea
                v-model="form.instructions"
                rows="6"
                maxlength="16000"
                required
              ></textarea></label
            ><label class="checkbox"
              ><input
                v-model="form.enabled"
                type="checkbox"
              />启用（对话时仍需显式选择）</label
            ><small>文字方法不会授予执行、发送或系统权限。</small></template
          ><label v-if="page === 'diary'"
            >引用的记忆来源<select v-model="form.sources" multiple>
              <option
                v-for="source in sources"
                :key="source.id"
                :value="source.id"
              >
                {{ source.data.content.slice(0, 50) }}
              </option></select
            ><small>最多16条。来源遗忘后，这篇派生日记也会失效。</small></label
          ><template v-if="page === 'memory'"
            ><label
              >可见范围<select v-model="visibility">
                <option value="private">私有</option>
                <option value="shared">共享给所有对话</option>
              </select></label
            ><label v-if="visibility === 'shared'" class="checkbox"
              ><input
                v-model="publish"
                type="checkbox"
                required
              />我确认这条资料可以向所有对话公开</label
            ></template
          >
          <div class="form-actions">
            <button
              type="button"
              class="secondary"
              :disabled="busy"
              @click="editor = false"
            >
              取消</button
            ><button class="primary" :disabled="busy">保存</button>
          </div>
        </form>
      </section>
    </div>
    <div v-if="runDetail" class="modal-backdrop" @click.self="runDetail = null">
      <section
        class="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="run-title"
      >
        <div class="section-heading">
          <h2 id="run-title">运行结果</h2>
          <button
            class="icon-button"
            aria-label="关闭运行结果"
            @click="runDetail = null"
          >
            ×
          </button>
        </div>
        <p>
          <span class="badge">{{
            labels[String(runDetail.status)] || runDetail.status
          }}</span>
        </p>
        <p class="record-body">
          {{ runDetail.response || runDetail.error || "请求尚未完成" }}
        </p>
        <small class="muted">{{ runDetail.id }}</small>
      </section>
    </div>
  </div>
</template>
