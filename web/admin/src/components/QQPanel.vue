<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { api, ApiError, errorMessage } from "../api";

interface Call {
  operation?: string;
  outcome: string;
  http_status?: number;
  business_code?: number;
  at?: number;
}
interface Operation {
  id: string;
  title: string;
  method: string;
  path: string;
  parameters: string[];
  query_fields: string[];
  required_query_fields?: string[];
  query_defaults?: Record<string, string>;
  body_fields: string[];
  required_body_fields: string[];
  category: string;
  read_only: boolean;
  deprecated: boolean;
  documentation: string;
  last_call?: Call | null;
}
const emit = defineEmits<{ unauthorized: [] }>();
const operations = ref<Operation[]>([]);
const selectedId = ref("get_users_me");
const selected = computed(() => operations.value.find((o) => o.id === selectedId.value));
const parameters = ref<Record<string, string>>({});
const query = ref<Record<string, string>>({});
const body = ref("{}");
const acknowledge = ref(false);
const busy = ref(false);
const error = ref("");
const notice = ref("");
const result = ref<unknown>(null);
const filter = ref("");
const category = ref("");
const calls = ref<Call[]>([]);
const labels: Record<string, string> = {
  bot: "机器人与菜单",
  c2c: "单聊与媒体",
  channel: "频道与私信",
  gateway: "连接与网关",
  media: "Markdown 素材",
};
const outcomes: Record<string, string> = {
  platform_accepted: "平台已接受",
  platform_permission_denied: "平台权限不足",
  platform_rejected: "平台已拒绝",
  asynchronous_pending: "平台异步处理中",
  result_unknown: "结果待核对",
  transport_failed: "连接失败",
  requires_resource: "缺少可测资源",
  not_executed_write: "未执行写入测试",
};
const visible = computed(() => operations.value.filter((o) => (!category.value || o.category === category.value) && `${o.title} ${o.path}`.toLowerCase().includes(filter.value.toLowerCase())));
const accepted = computed(() => operations.value.filter((o) => o.last_call?.outcome === "platform_accepted").length);
const permissions = computed(() => operations.value.filter((o) => o.last_call?.outcome === "platform_permission_denied").length);
const tested = computed(() => operations.value.filter((o) => !!o.last_call).length);
watch(selectedId, () => {
  parameters.value = {};
  query.value = { ...selected.value?.query_defaults };
  body.value = "{}";
  acknowledge.value = false;
  result.value = null;
  notice.value = error.value = "";
});
async function request<T>(path: string, options = {}) {
  try { return await api<T>(path, options); }
  catch (cause) { if (cause instanceof ApiError && cause.status === 401) emit("unauthorized"); throw cause; }
}
async function refresh() {
  const [catalog, recent] = await Promise.all([
    request<{ operations: Operation[] }>("/qq/api/catalog"),
    request<{ calls: Call[] }>("/qq/api/calls"),
  ]);
  operations.value = catalog.operations;
  calls.value = recent.calls;
}
async function load() {
  busy.value = true; error.value = "";
  try { await refresh(); } catch (cause) { error.value = errorMessage(cause); } finally { busy.value = false; }
}
async function probe() {
  busy.value = true; error.value = notice.value = "";
  try {
    result.value = await request("/qq/api/read-probe", { method: "POST", body: { parameters: {} } });
    await refresh();
    notice.value = "只读探测完成。需要频道、成员或消息标识的接口，请在高级操作中选择具体资源。";
  } catch (cause) { error.value = errorMessage(cause); } finally { busy.value = false; }
}
async function execute(dryRun: boolean) {
  if (!selected.value) return;
  error.value = notice.value = "";
  let parsed: Record<string, unknown> | undefined;
  try {
    if (!selected.value.read_only && body.value.trim()) {
      parsed = JSON.parse(body.value);
      if (!parsed || Array.isArray(parsed) || typeof parsed !== "object") throw new Error();
    }
  } catch { error.value = "请求内容须为有效 JSON 对象。"; return; }
  busy.value = true;
  try {
    result.value = await request(`/qq/api/operations/${selected.value.id}`, {
      method: "POST",
      body: { parameters: parameters.value, query: Object.fromEntries(Object.entries(query.value).filter(([, value]) => value !== "")), body: parsed, dry_run: dryRun, acknowledge_effects: acknowledge.value },
    });
    if (!dryRun) { await refresh(); acknowledge.value = false; }
    notice.value = dryRun ? "请求已校验，尚未调用 QQ。" : "本次调用结果已记录；发送消息是否在客户端可见，仍需结合 QQ 核对。";
  } catch (cause) { error.value = errorMessage(cause); } finally { busy.value = false; }
}
onMounted(load);
</script>

<template>
  <section class="qq-platform-panel">
    <header class="qq-platform-header">
      <div><h3>QQ 能力与接口</h3><p class="muted">查看当前账号的连接、平台权限和调用结果。</p></div>
      <button class="button secondary" :disabled="busy" @click="load">刷新</button>
    </header>
    <p v-if="error" role="alert" class="error">{{ error }}</p>
    <p v-if="notice" role="status" class="success">{{ notice }}</p>
    <div class="qq-platform-stats">
      <div><strong>{{ operations.length }}</strong><span>已接入接口</span></div>
      <div><strong>{{ accepted }}</strong><span>平台接受</span></div>
      <div><strong>{{ permissions }}</strong><span>权限不足</span></div>
      <div><strong>{{ operations.length - tested }}</strong><span>待真实测试</span></div>
    </div>
    <p class="muted">单聊支持逐句文字、图片、视频与本地合成语音；每条入站消息最多回复 4 次，回复之间按设置留出间隔。</p>
    <button class="button" :disabled="busy" @click="probe">{{ busy ? "处理中…" : "运行只读连接探测" }}</button>
    <p class="muted">探测只查询机器人、菜单、面板、素材和网关；不会自动发送消息或修改频道。</p>
    <details class="qq-platform-advanced">
      <summary>高级接口管理</summary>
      <p class="muted">固定的官方接口清单。每次写入需单独确认，模型聊天无法调用此面板。</p>
      <div class="qq-platform-filters">
        <label>范围<select v-model="category"><option value="">全部</option><option v-for="(label, key) in labels" :key="key" :value="key">{{ label }}</option></select></label>
        <label>搜索<input v-model="filter" placeholder="接口名称或路径" /></label>
      </div>
      <label class="qq-platform-select">操作<select v-model="selectedId"><option v-for="operation in visible" :key="operation.id" :value="operation.id">{{ operation.title }} · {{ operation.method }} {{ operation.path }}</option></select></label>
      <div v-if="selected" class="qq-platform-operation">
        <p><strong>{{ selected.title }}</strong> <a :href="selected.documentation" target="_blank" rel="noopener noreferrer">官方说明</a></p>
        <p class="muted">{{ selected.method }} {{ selected.path }}</p>
        <p v-if="selected.deprecated" class="error">官方已废弃此接口，保持禁用。</p>
        <p>最近状态：{{ selected.last_call ? outcomes[selected.last_call.outcome] || selected.last_call.outcome : "尚未真实测试" }}</p>
        <div class="qq-platform-fields"><label v-for="parameter in selected.parameters" :key="parameter">{{ parameter }}<input v-model="parameters[parameter]" autocomplete="off" /></label></div>
        <div class="qq-platform-fields"><label v-for="field in selected.query_fields" :key="field">{{ field }}（{{ selected.required_query_fields?.includes(field) ? "必填" : "可选" }}）<input v-model="query[field]" autocomplete="off" /></label></div>
        <template v-if="!selected.read_only && selected.body_fields.length">
          <label>JSON 请求内容<textarea v-model="body" rows="7" spellcheck="false" /></label>
          <p class="muted">允许字段：{{ selected.body_fields.join("、") }}<span v-if="selected.required_body_fields.length">；必填：{{ selected.required_body_fields.join("、") }}</span></p>
          <p class="muted">上传凭证与媒体 file_info 由服务端保管。后续引用形如 {"$qq_ref":"返回的 object_ref","field":"file_info"}。</p>
        </template>
        <label v-if="!selected.read_only" class="qq-platform-ack"><input v-model="acknowledge" type="checkbox" />我已核对目标和请求内容，执行本次 {{ selected.title }}</label>
        <div class="qq-platform-actions"><button class="button secondary" :disabled="busy" @click="execute(true)">仅校验请求</button><button class="button" :disabled="busy || selected.deprecated || (!selected.read_only && !acknowledge)" @click="execute(false)">{{ selected.read_only ? "查询" : "执行本次操作" }}</button></div>
      </div>
      <details v-if="calls.length"><summary>最近调用记录</summary><ul><li v-for="(call, index) in calls.slice(0, 15)" :key="index">{{ call.operation }} · {{ outcomes[call.outcome] || call.outcome }}<span v-if="call.http_status"> · HTTP {{ call.http_status }}</span><span v-if="call.business_code"> · {{ call.business_code }}</span></li></ul></details>
    </details>
    <details v-if="result" class="qq-platform-result"><summary>本次结果</summary><pre>{{ JSON.stringify(result, null, 2) }}</pre></details>
  </section>
</template>

<style scoped>
.qq-platform-panel { margin-top: 1.5rem; padding-top: 1rem; border-top: 1px solid var(--border, #ddd); }
.qq-platform-header { display: flex; justify-content: space-between; align-items: start; gap: 1rem; }
.qq-platform-header h3 { margin: 0; }
.qq-platform-stats { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: .75rem; margin: 1rem 0; }
.qq-platform-stats div { padding: .85rem; background: var(--surface-2, #f5f6f7); border-radius: .6rem; }
.qq-platform-stats strong { display: block; font-size: 1.35rem; }
.qq-platform-stats span { font-size: .85rem; }
.qq-platform-advanced, .qq-platform-result { margin-top: 1rem; }
.qq-platform-filters, .qq-platform-fields { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: .75rem; margin: 1rem 0; }
.qq-platform-panel label { display: grid; gap: .35rem; }
.qq-platform-panel select, .qq-platform-panel input, .qq-platform-panel textarea { width: 100%; min-width: 0; }
.qq-platform-panel textarea { font-family: ui-monospace, monospace; }
.qq-platform-operation { margin: 1rem 0; }
.qq-platform-ack { display: flex !important; align-items: center; margin: .75rem 0; }
.qq-platform-ack input { width: auto; }
.qq-platform-actions { display: flex; gap: .75rem; flex-wrap: wrap; }
.qq-platform-result pre { max-height: 30rem; overflow: auto; white-space: pre-wrap; word-break: break-word; font-size: .8rem; }
.qq-platform-panel .muted { color: var(--text-muted, #656b75); font-size: .88rem; }
@media (max-width: 700px) { .qq-platform-stats { grid-template-columns: repeat(2, minmax(0,1fr)); } .qq-platform-filters, .qq-platform-fields { grid-template-columns: 1fr; } }
</style>
