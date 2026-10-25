<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { api, ApiError, errorMessage } from "../api";

interface Version {
  id: string;
  plugin_id: string;
  version: string;
  status: string;
  revision: number;
  source_sha256: string | null;
  component_sha256: string | null;
  bundle_sha256: string | null;
  test_report: {
    passed: boolean;
    cases: Array<{ passed: boolean; actual: unknown }>;
  } | null;
}
interface Detail extends Version {
  specification: {
    plugin_id: string;
    version: string;
    description: string;
    input_schema: unknown;
    output_schema: unknown;
    tests: unknown[];
  };
  body: string | null;
}
interface Job {
  id: string;
  kind: string;
  status: string;
  revision: number;
  error_code: string | null;
  version_id: string;
}
interface Route {
  plugin_id: string;
  active_version: string | null;
  previous_version: string | null;
  revision: number;
}
interface Page {
  available: boolean;
  availability_code: string | null;
  engine: string;
  host_api: string;
  versions: Version[];
  jobs: Job[];
  routes: Route[];
}
const emit = defineEmits<{ unauthorized: [] }>();
const root = "/plugin-factory";
const page = ref<Page | null>(null);
const selected = ref<Detail | null>(null);
const pluginId = ref("user.integer-sum");
const version = ref("0.1.0");
const description = ref("输入一个整数数组，返回所有整数的和；空数组返回 0。");
const inputSchema = ref('{"type":"array","items":{"type":"integer"}}');
const outputSchema = ref('{"type":"integer"}');
const cases = ref(
  '[{"input":[2,3],"expected":5},{"input":[-4,1],"expected":-3}]',
);
const source = ref("");
const invocation = ref("[2,3]");
const output = ref("");
const error = ref("");
const notice = ref("");
const isolation = ref("");
const busy = ref(false);
const reviewed = ref(false);
const controller = new AbortController();
let timer: ReturnType<typeof setTimeout> | undefined;
let disposed = false;
const labels: Record<string, string> = {
  specified: "等待生成",
  generating: "正在生成",
  generated: "待构建",
  generation_failed: "生成失败",
  building: "正在构建",
  built: "待测试",
  build_failed: "构建失败",
  testing: "正在测试",
  test_failed: "测试失败",
  eligible: "测试通过，待批准",
  approved: "已批准，待启用",
  active: "已启用",
  quarantined: "已隔离",
  rolled_back: "已回退",
  queued: "排队中",
  running: "处理中",
  completed: "完成",
  failed: "失败",
  cancelled: "已取消",
  interrupted: "已中断",
  author: "录入源码",
  generate: "生成源码",
  build: "构建",
  test: "测试",
};
const route = computed(() =>
  page.value?.routes.find(
    (item) => item.plugin_id === selected.value?.plugin_id,
  ),
);
const pending = computed(
  () =>
    page.value?.jobs.some((item) =>
      ["queued", "running"].includes(item.status),
    ) ?? false,
);
const canBuild = computed(
  () =>
    !!page.value?.available &&
    !!selected.value &&
    ["generated", "build_failed"].includes(selected.value.status),
);
const canTest = computed(
  () =>
    !!page.value?.available &&
    !!selected.value &&
    ["built", "test_failed"].includes(selected.value.status),
);
async function load() {
  const result = await api<Page>(root, { signal: controller.signal });
  if (disposed) return;
  page.value = result;
  if (selected.value) {
    const current = result.versions.find(
      (item) => item.id === selected.value?.id,
    );
    if (current && current.revision !== selected.value.revision) {
      const detail = await api<Detail>(`${root}/versions/${current.id}`, {
        signal: controller.signal,
      });
      if (!disposed) {
        selected.value = detail;
        reviewed.value = false;
      }
    }
  }
}
async function action(fn: () => Promise<void>) {
  if (busy.value || disposed) return;
  busy.value = true;
  error.value = "";
  notice.value = "";
  try {
    await fn();
    if (!disposed) await load();
  } catch (caught) {
    if (!disposed) {
      if (caught instanceof ApiError && caught.status === 401)
        emit("unauthorized");
      else
        error.value =
          caught instanceof Error && !(caught instanceof ApiError)
            ? caught.message
            : errorMessage(caught);
    }
  } finally {
    busy.value = false;
  }
}
async function select(item: Version) {
  await action(async () => {
    selected.value = await api<Detail>(`${root}/versions/${item.id}`, {
      signal: controller.signal,
    });
    reviewed.value = false;
    output.value = "";
  });
}
async function create() {
  await action(async () => {
    let schemaInput: unknown, schemaOutput: unknown, tests: unknown;
    try {
      schemaInput = JSON.parse(inputSchema.value);
      schemaOutput = JSON.parse(outputSchema.value);
      tests = JSON.parse(cases.value);
    } catch {
      throw new Error("输入/输出契约和测试用例需要有效 JSON。");
    }
    selected.value = await api<Detail>(root, {
      method: "POST",
      body: {
        client_request_id: crypto.randomUUID(),
        specification: {
          plugin_id: pluginId.value.trim(),
          version: version.value.trim(),
          description: description.value,
          input_schema: schemaInput,
          output_schema: schemaOutput,
          tests,
        },
        body: source.value.trim() || null,
      },
      signal: controller.signal,
    });
    reviewed.value = false;
    notice.value = source.value.trim()
      ? "源码已保存，请审阅后单独构建。"
      : "已排队生成 Rust 源码；生成结果仍需审阅、构建和测试。";
  });
}
async function queue(kind: "build" | "test") {
  const item = selected.value;
  if (!item) return;
  await action(async () => {
    await api(`${root}/versions/${item.id}/${kind}`, {
      method: "POST",
      revision: item.revision,
      body: { client_request_id: crypto.randomUUID() },
      signal: controller.signal,
    });
    notice.value =
      kind === "build"
        ? "已排队隔离构建。"
        : "已排队执行独立测试，测试通过后仍需人工批准。";
  });
}
async function approve() {
  const item = selected.value;
  if (!item || !reviewed.value) return;
  await action(async () => {
    selected.value = {
      ...item,
      ...(await api<Version>(`${root}/versions/${item.id}/approve`, {
        method: "POST",
        revision: item.revision,
        body: {
          reviewed: true,
          source_sha256: item.source_sha256,
          component_sha256: item.component_sha256,
          bundle_sha256: item.bundle_sha256,
        },
        signal: controller.signal,
      })),
    };
    reviewed.value = false;
    notice.value = "此制品已批准，可单独启用。";
  });
}
async function activate(rollback = false, target?: string) {
  await action(async () => {
    const item = target
      ? await api<Detail>(`${root}/versions/${target}`, {
          signal: controller.signal,
        })
      : selected.value;
    if (!item) return;
    const registry = page.value?.routes.find(
      (value) => value.plugin_id === item.plugin_id,
    );
    const value = await api<Version>(
      `${root}/versions/${item.id}/${rollback ? "rollback" : "activate"}`,
      {
        method: "POST",
        revision: item.revision,
        body: {
          route_revision: registry?.revision || 0,
          bundle_sha256: item.bundle_sha256,
        },
        signal: controller.signal,
      },
    );
    selected.value = { ...item, ...value };
    reviewed.value = false;
    notice.value = rollback
      ? "已回退至此前批准的版本。"
      : "已启用此版本；后续调用使用此制品与权限版本。";
  });
}
async function quarantine() {
  const item = selected.value;
  if (!item) return;
  await action(async () => {
    selected.value = {
      ...item,
      ...(await api<Version>(`${root}/versions/${item.id}/quarantine`, {
        method: "POST",
        revision: item.revision,
        body: {},
        signal: controller.signal,
      })),
    };
    notice.value = "已撤销此版本的调用路由并隔离。";
  });
}
async function invoke() {
  const item = selected.value;
  if (!item) return;
  await action(async () => {
    let input: unknown;
    try {
      input = JSON.parse(invocation.value);
    } catch {
      throw new Error("调用参数需要有效 JSON。");
    }
    const value = await api<{ output: unknown }>(
      `${root}/plugins/${encodeURIComponent(item.plugin_id)}/invoke`,
      { method: "POST", body: { input }, signal: controller.signal },
    );
    output.value = JSON.stringify(value.output, null, 2);
  });
}
async function check() {
  await action(async () => {
    const result = await api<{
      passed: boolean;
      isolation: unknown;
      probes: unknown;
    }>(`${root}/check`, {
      method: "POST",
      body: {},
      signal: controller.signal,
    });
    isolation.value = JSON.stringify(result, null, 2);
    notice.value = result.passed
      ? "本次工作器隔离探针通过；每次构建与调用仍会重新检查。"
      : "隔离探针未通过，构建保持不可用。";
  });
}
async function cancel(job: Job) {
  await action(async () => {
    await api(`${root}/jobs/${job.id}/cancel`, {
      method: "POST",
      revision: job.revision,
      body: {},
      signal: controller.signal,
    });
    notice.value = "已请求取消，工作器退出前继续占用执行名额。";
  });
}
async function poll() {
  try {
    if (!busy.value) await load();
  } catch (caught) {
    if (!disposed && caught instanceof ApiError && caught.status === 401)
      emit("unauthorized");
  }
  if (!disposed) timer = setTimeout(poll, pending.value ? 1500 : 10000);
}
onMounted(() => {
  void action(load);
  timer = setTimeout(poll, 1500);
});
onUnmounted(() => {
  disposed = true;
  controller.abort();
  if (timer) clearTimeout(timer);
});
</script>

<template>
  <section class="plugin-factory" aria-label="Rust 插件工厂">
    <header class="factory-heading">
      <div>
        <h2>Rust 插件工厂</h2>
        <p>把需求生成为有版本的工具，审阅、构建、测试后分别批准与启用。</p>
      </div>
      <button class="button" :disabled="busy" @click="check">检查构建环境</button>
    </header>
    <p class="muted">
      当前策略支持无外部权限的无状态 JSON
      工具。网络、记忆、消息发送和进程能力尚不授予插件；源码和调用结果仅当前管理账户可见。
    </p>
    <p v-if="page && !page.available" role="status">
      隔离构建器或 Rust WASM 工具链尚不可用。可保存需求，构建保持关闭。
    </p>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <p v-if="notice" role="status">{{ notice }}</p>
    <details v-if="isolation">
      <summary>本次隔离检查结果</summary>
      <pre>{{ isolation }}</pre>
    </details>
    <div class="factory-columns">
      <form class="factory-card" @submit.prevent="create">
        <h3>新建版本</h3>
        <label
          >插件 ID<input
            v-model="pluginId"
            placeholder="user.integer-sum"
            required
            maxlength="64"
            :disabled="busy"
        /></label>
        <label
          >版本<input
            v-model="version"
            placeholder="0.1.0"
            required
            :disabled="busy"
        /></label>
        <label
          >需要什么功能<textarea
            v-model="description"
            rows="4"
            required
            :disabled="busy"
          />
        </label>
        <details>
          <summary>输入、输出与测试契约</summary>
          <label
            >输入 JSON Schema<textarea
              v-model="inputSchema"
              rows="3"
              :disabled="busy"
            />
          </label>
          <label
            >输出 JSON Schema<textarea
              v-model="outputSchema"
              rows="3"
              :disabled="busy"
            />
          </label>
          <label
            >测试用例（2–8 个）<textarea
              v-model="cases"
              rows="5"
              :disabled="busy"
            />
          </label>
          <p class="muted">
            支持明确 type、properties、required、additionalProperties、items 和
            enum；其他关键字会被拒绝。
          </p>
        </details>
        <details>
          <summary>已有 Rust 实现</summary>
          <label
            >函数体（留空则由模型生成）<textarea
              v-model="source"
              rows="7"
              spellcheck="false"
              :disabled="busy"
            />
          </label>
          <p class="muted">
            固定签名为 fn plugin(input: &amp;str) → String；输入与返回均为 JSON
            字符串。使用标准库，无第三方依赖。
          </p>
        </details>
        <button class="button" type="submit" :disabled="busy || pending">
          {{ source.trim() ? "保存源码版本" : "生成 Rust 源码" }}
        </button>
      </form>
      <div class="factory-card">
        <h3>版本与任务</h3>
        <p v-if="!page?.versions.length" class="muted">尚未创建插件版本。</p>
        <button
          v-for="item in page?.versions"
          :key="item.id"
          class="factory-version"
          :aria-pressed="selected?.id === item.id"
          :disabled="busy"
          @click="select(item)"
        >
          <strong>{{ item.plugin_id }} · {{ item.version }}</strong
          ><span>{{ labels[item.status] || item.status }}</span>
        </button>
        <details v-if="page?.jobs.length">
          <summary>最近任务</summary>
          <ul>
            <li v-for="job in page.jobs.slice(0, 12)" :key="job.id">
              {{ labels[job.kind] || job.kind }}：{{
                labels[job.status] || job.status
              }}
              <code v-if="job.error_code">{{ job.error_code }}</code
              ><button class="button"
                v-if="['queued', 'running'].includes(job.status)"
                :disabled="busy"
                @click="cancel(job)"
              >
                取消
              </button>
            </li>
          </ul>
        </details>
      </div>
    </div>
    <article v-if="selected" class="factory-card factory-detail">
      <h3>{{ selected.plugin_id }} · {{ selected.version }}</h3>
      <p>{{ labels[selected.status] || selected.status }}</p>
      <p>{{ selected.specification.description }}</p>
      <details
        :open="
          selected.status === 'generated' || selected.status === 'eligible'
        "
      >
        <summary>审阅 Rust 源码、契约与制品</summary>
        <pre v-if="selected.body">{{ selected.body }}</pre>
        <pre>{{ JSON.stringify(selected.specification, null, 2) }}</pre>
        <dl>
          <dt>源码 SHA-256</dt>
          <dd>
            <code>{{ selected.source_sha256 || "尚未生成" }}</code>
          </dd>
          <dt>Component SHA-256</dt>
          <dd>
            <code>{{ selected.component_sha256 || "尚未构建" }}</code>
          </dd>
          <dt>发布包 SHA-256</dt>
          <dd>
            <code>{{ selected.bundle_sha256 || "尚未构建" }}</code>
          </dd>
        </dl>
      </details>
      <details v-if="selected.test_report">
        <summary>独立测试结果</summary>
        <pre>{{ JSON.stringify(selected.test_report, null, 2) }}</pre>
      </details>
      <div class="factory-actions">
        <button class="button"
          :disabled="busy || pending || !canBuild"
          @click="queue('build')"
        >
          隔离构建</button
        ><button class="button" :disabled="busy || pending || !canTest" @click="queue('test')">
          运行测试
        </button>
      </div>
      <div v-if="selected.status === 'eligible'" class="factory-approval">
        <label
          ><input
            v-model="reviewed"
            type="checkbox"
            :disabled="busy"
          />我已审阅本版本源码、契约、测试和摘要，批准空权限制品</label
        ><button class="button" :disabled="busy || !reviewed" @click="approve">
          批准此制品
        </button>
      </div>
      <div class="factory-actions">
        <button class="button"
          v-if="['approved', 'rolled_back'].includes(selected.status)"
          :disabled="busy"
          @click="activate()"
        >
          启用此版本</button
        ><button class="button"
          v-if="route?.previous_version"
          :disabled="busy"
          @click="activate(true, route.previous_version || undefined)"
        >
          回滚到上一版本</button
        ><button class="button"
          v-if="
            !['quarantined', 'specified', 'generating'].includes(
              selected.status,
            )
          "
          :disabled="busy"
          @click="quarantine"
        >
          撤销并隔离
        </button>
      </div>
      <form v-if="selected.status === 'active'" @submit.prevent="invoke">
        <h4>调用已启用工具</h4>
        <label
          >JSON 输入<textarea
            v-model="invocation"
            rows="3"
            :disabled="busy"
          /></label
        ><button class="button" :disabled="busy">调用工具</button>
        <pre v-if="output" aria-label="插件输出">{{ output }}</pre>
      </form>
    </article>
  </section>
</template>

<style scoped>
.plugin-factory {
  display: grid;
  gap: 1rem;
}
.factory-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
}
.factory-heading h2,
.factory-card h3 {
  margin-top: 0;
}
.factory-columns {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  gap: 1rem;
}
.factory-card {
  padding: 1.25rem;
  border: 1px solid var(--border, #ddd);
  border-radius: 14px;
  min-width: 0;
}
.factory-card label {
  display: grid;
  gap: 0.4rem;
  margin: 0.8rem 0;
}
.factory-card input:not([type="checkbox"]),
.factory-card textarea {
  width: 100%;
  box-sizing: border-box;
}
.factory-card textarea,
pre {
  font-family: ui-monospace, monospace;
}
.factory-actions {
  display: flex;
  gap: 0.6rem;
  flex-wrap: wrap;
  margin: 1rem 0;
}
.factory-version {
  display: flex;
  justify-content: space-between;
  align-items: center;
  width: 100%;
  gap: 1rem;
  margin: 0.5rem 0;
  padding: 0.8rem;
  text-align: left;
}
.factory-version[aria-pressed="true"] {
  border-color: var(--accent, #6495ed);
}
.factory-approval {
  padding: 1rem;
  background: var(--surface-muted, #f5f6f8);
  border-radius: 10px;
}
.factory-approval label {
  display: flex;
  align-items: flex-start;
  gap: 0.5rem;
}
.factory-card dd {
  margin: 0.3rem 0 1rem;
  overflow-wrap: anywhere;
}
.factory-card details {
  margin: 0.8rem 0;
}
pre {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  padding: 0.8rem;
  background: var(--surface-muted, #f5f6f8);
  border-radius: 8px;
  max-height: 28rem;
  overflow: auto;
}
.muted {
  color: var(--text-muted, #666);
}
.error {
  color: var(--danger, #b02727);
}
@media (max-width: 900px) {
  .factory-columns {
    grid-template-columns: 1fr;
  }
  .factory-heading {
    align-items: flex-start;
    flex-direction: column;
  }
}
</style>
