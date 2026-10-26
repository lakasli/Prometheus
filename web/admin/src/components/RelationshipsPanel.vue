<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, useId } from "vue";
import { api, errorMessage } from "../api";
import AdminSelect from "./AdminSelect.vue";
import type { Conversation } from "../types";

interface Person {
  id: string;
  display_name: string;
  revision: number;
  visibility: string;
  source_owner_id: string | null;
  source_conversation_id: string | null;
  merged_into: string | null;
}
interface Assertion {
  id: string;
  person_id: string;
  field: string;
  value: unknown;
  revision: number;
  status: string;
  statement_kind: string;
  source_type: string;
  source_id: string;
  source_revision: number;
  reason: string;
  valid_until: number | null;
}
interface Edge {
  id: string;
  from_person_id: string;
  to_person_id: string;
  relationship_type: string;
  revision: number;
  status: string;
  statement_kind: string;
  reason: string;
}
interface Detail extends Person {
  assertions: Assertion[];
  identities: {
    id: string;
    person_id: string;
    conversation_id: string;
    alias: string;
  }[];
  affinity: {
    score: number;
    daily_decay: number;
    grace_seconds: number;
    last_direct_at: number | null;
  };
  contact_permission: {
    revision: number;
    status: string;
    delivery_available: boolean;
    blocked_reason: string;
  };
  identity_group: {
    id: string;
    revision: number;
    members: Person[];
    mapping_scope: string;
  } | null;
}
interface ModelJob {
  id: string;
  person_id: string;
  status: string;
  error: string | null;
  source_id: string;
  result: { assertion_ids?: string[]; edge_ids?: string[] };
}
interface AffinityPolicy {
  revision: number;
  initial: number;
  minimum: number;
  maximum: number;
  daily_decay: number;
  grace_seconds: number;
  downtime_cap_seconds: number;
  effective_at: number;
}
const loadedPolicy = ref<AffinityPolicy | null>(null);
const policyForm = reactive({
  initial: 50,
  minimum: 0,
  maximum: 100,
  daily_decay: 1,
  grace_seconds: 259200,
  downtime_cap_seconds: 604800,
  confirmed: false,
  reason: "",
});
const previewDays = ref(10);
const policyPreview = ref<number | null>(null);
const modelSources = ref<{ id: string; revision: number; content: string }[]>(
  [],
);
const modelJobs = ref<ModelJob[]>([]);
const modelForm = reactive({
  source: "",
  confirmed: false,
  targets: [] as string[],
});
const uid = useId().replace(/:/g, "");
const people = ref<Person[]>([]);
const relationships = ref<Edge[]>([]);
const conversations = ref<Conversation[]>([]);
const selected = ref<Detail | null>(null);
const loading = ref(false);
const busy = ref(false);
const error = ref("");
const notice = ref("");
const search = ref("");
const total = ref(0);
const offset = ref(0);
const creating = ref(false);
const deleteTarget = ref<Person | Assertion | Edge | null>(null);
const deleteKind = ref<"person" | "assertion" | "edge">("person");
const createForm = reactive({ display_name: "", source: "", reason: "" });
const assertionForm = reactive({
  field: "hobby",
  value: "",
  statement: "self_report",
  reason: "",
  source_type: "manual",
  source_id: "",
  source_revision: 1,
});
const reviewReason = ref("");
const supersedes = ref<string[]>([]);
const identityForm = reactive({
  conversation_id: "",
  alias: "",
  reason: "",
  confirmed: false,
});
const mergeForm = reactive({ target: "", reason: "", confirmed: false });
const groupForm = reactive({ other: "", reason: "", confirmed: false });
const edgeForm = reactive({
  to: "",
  type: "friend",
  reason: "",
  statement: "third_party",
});
const affinityForm = reactive({ delta: 1, reason: "" });
const renameForm = reactive({ name: "", reason: "" });
let loadController: AbortController | null = null;
let detailController: AbortController | null = null;
let disposed = false;
let detailEpoch = 0;
const fieldNames: Record<string, string> = {
  name: "姓名",
  nickname: "昵称",
  age: "年龄",
  birthday: "生日",
  job: "职业",
  personality: "性格",
  hobby: "爱好",
  preference: "偏好",
  commitment: "约定",
};
const statements: Record<string, string> = {
  self_report: "本人陈述",
  third_party: "第三方陈述",
  inference: "推断",
  confirmed: "人工确认",
};
const edgeNames: Record<string, string> = {
  family: "家人",
  friend: "朋友",
  colleague: "同事",
  acquaintance: "初识",
  mentioned: "仅被提及",
  other: "其他",
};
const statuses: Record<string, string> = {
  pending: "待审核",
  approved: "已审核",
  rejected: "已驳回",
  superseded: "已替代",
};
const activePeople = computed(() => people.value.filter((p) => !p.merged_into));
const compatiblePeople = computed(() =>
  activePeople.value.filter(
    (p) =>
      p.id !== selected.value?.id &&
      p.source_owner_id === selected.value?.source_owner_id &&
      p.visibility === selected.value?.visibility,
  ),
);
const graphPeople = computed(() => activePeople.value.slice(0, 20));
const positions = computed(
  () =>
    new Map(
      graphPeople.value.map((p, index) => [
        p.id,
        { x: 100 + (index % 4) * 180, y: 65 + Math.floor(index / 4) * 115 },
      ]),
    ),
);
const graphEdges = computed(() =>
  relationships.value.filter(
    (e) =>
      e.status === "approved" &&
      positions.value.has(e.from_person_id) &&
      positions.value.has(e.to_person_id),
  ),
);
const graphHeight = computed(() =>
  Math.max(150, Math.ceil(graphPeople.value.length / 4) * 115),
);
const currentAssertions = computed(
  () => selected.value?.assertions.filter((a) => a.status === "approved") || [],
);
function modelStatus(status: string) {
  const names: Record<string, string> = {
    queued: "排队中",
    running: "整理中",
    completed: "候选已生成",
    failed: "整理失败",
    cancelled: "已取消",
  };
  return names[status] || status;
}
function name(id: string) {
  return (
    people.value.find((p) => p.id === id)?.display_name || "当前页之外的人物"
  );
}
function display(value: unknown) {
  return typeof value === "string" ? value : JSON.stringify(value);
}
function personPath(id: string) {
  return `/relationships/people/${encodeURIComponent(id)}`;
}
function conversationName(id: string | null) {
  return (
    conversations.value.find((c) => c.id === id)?.title ||
    id ||
    "管理者直接录入"
  );
}
async function load() {
  loadController?.abort();
  const controller = new AbortController();
  loadController = controller;
  loading.value = true;
  error.value = "";
  try {
    const query = new URLSearchParams({
      limit: "50",
      offset: String(offset.value),
      include_merged: "true",
      q: search.value,
    });
    const [p, e, c] = await Promise.all([
      api<{ items: Person[]; total: number }>(
        `/relationships/people?${query}`,
        { signal: controller.signal },
      ),
      api<{ items: Edge[] }>("/relationships/edges", {
        signal: controller.signal,
      }),
      api<{ items: Conversation[] }>("/conversations", {
        signal: controller.signal,
      }),
    ]);
    if (disposed || controller.signal.aborted) return;
    people.value = p.items;
    total.value = p.total;
    relationships.value = e.items;
    conversations.value = c.items;
  } catch (e) {
    if (!controller.signal.aborted && !disposed) error.value = errorMessage(e);
  } finally {
    if (loadController === controller && !disposed) loading.value = false;
  }
}
async function choose(person: Person) {
  detailController?.abort();
  const controller = new AbortController();
  detailController = controller;
  const epoch = ++detailEpoch;
  error.value = "";
  try {
    const item = await api<Detail>(personPath(person.id), {
      signal: controller.signal,
    });
    if (disposed || controller.signal.aborted || epoch !== detailEpoch) return;
    if (selected.value?.id !== item.id) {
      modelSources.value = [];
      modelJobs.value = [];
      modelForm.source = "";
      modelForm.targets = [];
      modelForm.confirmed = false;
    }
    selected.value = item;
    renameForm.name = item.display_name;
    renameForm.reason = "";
    supersedes.value = [];
  } catch (e) {
    if (!controller.signal.aborted && !disposed) error.value = errorMessage(e);
  }
}
async function perform(
  operation: () => Promise<unknown>,
  message: string,
  refreshSelected = true,
) {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  notice.value = "";
  try {
    await operation();
    if (disposed) return;
    notice.value = message;
    await load();
    if (refreshSelected && selected.value) await choose(selected.value);
  } catch (e) {
    if (!disposed) error.value = errorMessage(e);
  } finally {
    if (!disposed) busy.value = false;
  }
}
async function createPerson() {
  if (!createForm.display_name.trim() || !createForm.reason.trim()) {
    error.value = "请填写人物称呼与建立依据。";
    return;
  }
  await perform(async () => {
    const item = await api<Person>("/relationships/people", {
      method: "POST",
      body: {
        display_name: createForm.display_name,
        visibility: "private",
        source_conversation_id: createForm.source || null,
        publish: false,
        reason: createForm.reason,
      },
    });
    creating.value = false;
    createForm.display_name = "";
    createForm.reason = "";
    await choose(item);
  }, "人物已建立。相同称呼仍保留独立身份。");
}
async function propose() {
  if (
    !selected.value ||
    !assertionForm.value.trim() ||
    !assertionForm.reason.trim()
  ) {
    error.value = "请填写字段值与可追溯依据。";
    return;
  }
  let value: unknown = assertionForm.value;
  if (["age", "birthday"].includes(assertionForm.field)) {
    try {
      value = JSON.parse(assertionForm.value);
    } catch {
      error.value = "年龄和生日须填写符合下方示例的 JSON。";
      return;
    }
  }
  await perform(
    () =>
      api(`${personPath(selected.value!.id)}/assertions`, {
        method: "POST",
        revision: selected.value!.revision,
        body: {
          field: assertionForm.field,
          value,
          evidence: {
            source_type: assertionForm.source_type,
            source_id:
              assertionForm.source_type === "manual"
                ? null
                : assertionForm.source_id,
            source_revision:
              assertionForm.source_type === "manual"
                ? null
                : assertionForm.source_revision,
            statement_kind: assertionForm.statement,
            reason: assertionForm.reason,
          },
          valid_from: null,
          valid_until: null,
        },
      }),
    "资料候选已保存，审核前不会进入对话。",
  );
}
async function review(item: Assertion, decision: "approve" | "reject") {
  if (!reviewReason.value.trim()) {
    error.value = "请填写本次审核理由。";
    return;
  }
  await perform(
    () =>
      api(`/relationships/assertions/${encodeURIComponent(item.id)}/review`, {
        method: "POST",
        revision: item.revision,
        body: {
          decision,
          supersedes: decision === "approve" ? supersedes.value : [],
          reason: reviewReason.value,
        },
      }),
    "候选审核已保存。",
  );
}
async function rename() {
  if (!selected.value || !renameForm.reason.trim()) {
    error.value = "更正称呼需要填写依据。";
    return;
  }
  await perform(
    () =>
      api(personPath(selected.value!.id), {
        method: "PATCH",
        revision: selected.value!.revision,
        body: { display_name: renameForm.name, reason: renameForm.reason },
      }),
    "称呼已更正，使用旧资料的上下文已失效。",
  );
}
async function bind() {
  if (
    !selected.value ||
    !identityForm.confirmed ||
    !identityForm.reason.trim()
  ) {
    error.value = "请核验会话身份、填写依据并明确确认。";
    return;
  }
  await perform(
    () =>
      api(`${personPath(selected.value!.id)}/identities`, {
        method: "POST",
        revision: selected.value!.revision,
        body: identityForm,
      }),
    "身份映射已保存。",
  );
}
async function merge() {
  const target = people.value.find((p) => p.id === mergeForm.target);
  if (
    !selected.value ||
    !target ||
    !mergeForm.confirmed ||
    !mergeForm.reason.trim()
  ) {
    error.value = "合并须选择同域目标、填写身份依据并明确确认。";
    return;
  }
  await perform(
    () =>
      api(`${personPath(selected.value!.id)}/merge`, {
        method: "POST",
        revision: selected.value!.revision,
        body: {
          target_person_id: target.id,
          target_revision: target.revision,
          confirmed: true,
          reason: mergeForm.reason,
        },
      }),
    "身份映射已合并；原始来源和独立好感事件保留，可拆分恢复。",
  );
}
async function split() {
  if (!selected.value || !mergeForm.confirmed || !mergeForm.reason.trim()) {
    error.value = "拆分须填写依据并明确确认。";
    return;
  }
  await perform(
    () =>
      api(`${personPath(selected.value!.id)}/split`, {
        method: "POST",
        revision: selected.value!.revision,
        body: { confirmed: true, reason: mergeForm.reason },
      }),
    "映射已拆分，原始资料与身份归属已恢复。",
  );
}
async function linkIdentityGroup() {
  const other = people.value.find((p) => p.id === groupForm.other);
  if (
    !selected.value ||
    !other ||
    !groupForm.confirmed ||
    !groupForm.reason.trim()
  ) {
    error.value = "跨账号映射须选择已核验档案、填写依据并明确确认。";
    return;
  }
  await perform(
    () =>
      api(`${personPath(selected.value!.id)}/identity-group`, {
        method: "POST",
        revision: selected.value!.revision,
        body: {
          other_person_id: other.id,
          other_revision: other.revision,
          confirmed: true,
          reason: groupForm.reason,
        },
      }),
    "跨账号同人映射已保存。每个来源的字段权限保持独立。",
  );
}
async function unlinkIdentityGroup() {
  if (!selected.value || !groupForm.confirmed || !groupForm.reason.trim()) {
    error.value = "解除跨账号映射须填写依据并明确确认。";
    return;
  }
  await perform(
    () =>
      api(`${personPath(selected.value!.id)}/identity-group`, {
        method: "DELETE",
        revision: selected.value!.revision,
        body: { confirmed: true, reason: groupForm.reason },
      }),
    "跨账号映射已解除，原始来源与资料归属保留。",
  );
}
async function createEdge() {
  if (!selected.value || !edgeForm.to || !edgeForm.reason.trim()) {
    error.value = "请选择关系目标并填写来源依据。";
    return;
  }
  await perform(
    () =>
      api("/relationships/edges", {
        method: "POST",
        body: {
          from_person_id: selected.value!.id,
          to_person_id: edgeForm.to,
          relationship_type: edgeForm.type,
          evidence: {
            source_type: "manual",
            source_id: null,
            source_revision: null,
            statement_kind: edgeForm.statement,
            reason: edgeForm.reason,
          },
        },
      }),
    "有向关系候选已保存，不自动创建反向关系。",
  );
}
async function reviewEdge(item: Edge, decision: string) {
  if (!reviewReason.value.trim()) {
    error.value = "请填写本次审核理由。";
    return;
  }
  await perform(
    () =>
      api(`/relationships/edges/${encodeURIComponent(item.id)}/review`, {
        method: "POST",
        revision: item.revision,
        body: { decision, supersedes: [], reason: reviewReason.value },
      }),
    "关系审核已保存。",
  );
}
async function adjustAffinity() {
  if (!selected.value || !affinityForm.reason.trim()) {
    error.value = "人工调整须填写依据。";
    return;
  }
  await perform(
    () =>
      api(`${personPath(selected.value!.id)}/affinity/events`, {
        method: "POST",
        revision: selected.value!.revision,
        body: {
          event_key: crypto.randomUUID(),
          kind: "manual_adjustment",
          delta: Number(affinityForm.delta),
          evidence: {
            source_type: "manual",
            source_id: null,
            source_revision: null,
            statement_kind: "confirmed",
            reason: affinityForm.reason,
          },
        },
      }),
    "好感变化已留存事件；数值不代表联系许可。",
  );
}
async function revokeContact() {
  if (!selected.value) return;
  await perform(
    () =>
      api(`${personPath(selected.value!.id)}/contact-permission`, {
        method: "PUT",
        revision: selected.value!.contact_permission.revision,
        body: {
          status: "revoked",
          conversation_id: null,
          expires_at: null,
          evidence: "管理页面明确撤销主动联系许可",
        },
      }),
    "主动联系许可已撤销。",
  );
}
async function loadModelSources() {
  if (!selected.value) return;
  await perform(
    async () => {
      const params = new URLSearchParams({ limit: "30", offset: "0" });
      if (selected.value?.source_owner_id)
        params.set("source_owner_id", selected.value.source_owner_id);
      const [events, jobs] = await Promise.all([
        api<{ items: { id: string; revision: number; content: string }[] }>(
          `/diary/events?${params}`,
        ),
        api<{ items: ModelJob[] }>(
          `/relationships/model-jobs?person_id=${encodeURIComponent(selected.value!.id)}`,
        ),
      ]);
      modelSources.value = events.items;
      modelJobs.value = jobs.items;
    },
    "可选来源已加载。请选择本次允许发送给当前模型的一条日记事件。",
    false,
  );
}
async function generateCandidates() {
  const source = modelSources.value.find(
    (item) => item.id === modelForm.source,
  );
  if (!selected.value || !source || !modelForm.confirmed) {
    error.value = "请选择日记来源并明确确认本次模型出域。";
    return;
  }
  await perform(async () => {
    const job = await api<ModelJob>("/relationships/model-jobs", {
      method: "POST",
      revision: selected.value!.revision,
      body: {
        person_id: selected.value!.id,
        source_type: "diary_event",
        source_id: source.id,
        source_revision: source.revision,
        target_person_ids: modelForm.targets,
        confirmed: true,
        idempotency_key: crypto.randomUUID(),
      },
    });
    modelJobs.value.unshift(job);
  }, "人物整理已排队。结果只生成待审核候选，不自动批准或合并身份。");
}
async function refreshModelJobs() {
  if (!selected.value) return;
  await perform(async () => {
    const jobs = await api<{ items: ModelJob[] }>(
      `/relationships/model-jobs?person_id=${encodeURIComponent(selected.value!.id)}`,
    );
    modelJobs.value = jobs.items;
  }, "整理进度已刷新。");
}
async function loadPolicy() {
  await perform(
    async () => {
      const policy = await api<AffinityPolicy>(
        "/relationships/affinity-policy",
      );
      loadedPolicy.value = policy;
      Object.assign(policyForm, {
        initial: policy.initial,
        minimum: policy.minimum,
        maximum: policy.maximum,
        daily_decay: policy.daily_decay,
        grace_seconds: policy.grace_seconds,
        downtime_cap_seconds: policy.downtime_cap_seconds,
        confirmed: false,
        reason: "",
      });
      policyPreview.value = null;
    },
    "当前好感规则已载入。",
    false,
  );
}
async function previewPolicy() {
  if (!policyForm.reason.trim()) {
    error.value = "请先填写规则调整依据。";
    return;
  }
  await perform(
    async () => {
      const result = await api<{ projected_from_initial: number }>(
        "/relationships/affinity-policy/preview",
        {
          method: "POST",
          body: {
            policy: policyForm,
            idle_seconds: Math.round(previewDays.value * 86400),
          },
        },
      );
      policyPreview.value = result.projected_from_initial;
    },
    "预演完成，未修改实际分数或联系许可。",
    false,
  );
}
async function savePolicy() {
  if (
    !loadedPolicy.value ||
    !policyForm.confirmed ||
    !policyForm.reason.trim()
  ) {
    error.value = "规则变更须填写依据并明确确认。";
    return;
  }
  await perform(async () => {
    loadedPolicy.value = await api<AffinityPolicy>(
      "/relationships/affinity-policy",
      {
        method: "PUT",
        revision: loadedPolicy.value!.revision,
        body: policyForm,
      },
    );
    policyForm.confirmed = false;
  }, "新规则已生效，旧规则结算区间和历史版本保留。");
}
async function confirmDelete() {
  const target = deleteTarget.value;
  if (!target) return;
  const kind = deleteKind.value;
  await perform(
    async () => {
      const path =
        kind === "person"
          ? personPath(target.id)
          : `/relationships/${kind === "assertion" ? "assertions" : "edges"}/${encodeURIComponent(target.id)}`;
      await api(path, { method: "DELETE", revision: target.revision });
      deleteTarget.value = null;
      if (kind === "person") selected.value = null;
    },
    "遗忘已生效，正文与旧版本内容已清除，相关上下文已失效。离线备份另按保留流程处理。",
    kind !== "person",
  );
}
function forget(
  target: Person | Assertion | Edge,
  kind: "person" | "assertion" | "edge",
) {
  deleteTarget.value = target;
  deleteKind.value = kind;
}
onMounted(load);
onUnmounted(() => {
  disposed = true;
  loadController?.abort();
  detailController?.abort();
});
</script>

<template>
  <section class="relationships-panel life-system">
    <div class="section-heading">
      <div>
        <h2>人际关系</h2>
        <p>
          人物按稳定身份保存，资料与有向关系逐项保留来源。同名不会自动合并。
        </p>
      </div>
      <button type="button" :disabled="busy" @click="creating = !creating">
        建立人物
      </button>
    </div>
    <p v-if="error" class="error" role="alert">{{ error }}</p>
    <p v-if="notice" class="success" role="status">{{ notice }}</p>
    <form
      v-if="creating"
      class="card relationship-form"
      @submit.prevent="createPerson"
    >
      <h3>建立独立人物</h3>
      <label
        >称呼<input v-model="createForm.display_name" maxlength="100" required
      /></label>
      <label
        >私有来源<AdminSelect
          v-model="createForm.source"
          :options="[
            { value: '', label: '管理者直接录入' },
            ...conversations.map((c) => ({ value: c.id, label: c.title })),
          ]"
      /></label>
      <label
        >建立依据<textarea
          v-model="createForm.reason"
          required
          maxlength="1000"
        />
      </label>
      <p class="muted">
        默认仅来源账号域可见。跨渠道身份须单独核验；未知资料保留为空。
      </p>
      <button type="submit" :disabled="busy">保存人物</button>
    </form>
    <form
      class="relationship-search"
      @submit.prevent="
        offset = 0;
        load();
      "
    >
      <label>查找人物<input v-model="search" placeholder="按称呼查找" /></label
      ><button type="submit" :disabled="loading || busy">查询</button
      ><button type="button" :disabled="loading || busy" @click="load">
        刷新
      </button>
    </form>
    <details class="card">
      <summary>好感规则与停机预演</summary>
      <button type="button" :disabled="busy" @click="loadPolicy">
        载入好感规则
      </button>
      <form
        v-if="loadedPolicy"
        class="relationship-form"
        @submit.prevent="savePolicy"
      >
        <p>
          当前版本 v{{
            loadedPolicy.revision
          }}。新规则只作用于生效水位之后；好感不会改变基础回复、纠错、隐私权或接收许可。
        </p>
        <label
          >新档案初始分<input
            v-model.number="policyForm.initial"
            type="number"
            min="0"
            max="100"
            required /></label
        ><label
          >下界<input
            v-model.number="policyForm.minimum"
            type="number"
            min="0"
            max="99"
            required /></label
        ><label
          >上界<input
            v-model.number="policyForm.maximum"
            type="number"
            min="1"
            max="100"
            required /></label
        ><label
          >每日空闲衰减<input
            v-model.number="policyForm.daily_decay"
            type="number"
            min="0"
            max="10"
            step="0.001"
            required /></label
        ><label
          >直接互动后宽限秒数<input
            v-model.number="policyForm.grace_seconds"
            type="number"
            min="0"
            max="2592000"
            required /></label
        ><label
          >未观测停机区间最多补算秒数<input
            v-model.number="policyForm.downtime_cap_seconds"
            type="number"
            min="0"
            max="2592000"
            required /></label
        ><label>调整依据<textarea v-model="policyForm.reason" required /></label
        ><label
          >预演空闲天数<input
            v-model.number="previewDays"
            type="number"
            min="0"
            max="3650"
            step="0.1" /></label
        ><button type="button" :disabled="busy" @click="previewPolicy">
          预演分数
        </button>
        <p v-if="policyPreview !== null" role="status">
          从初始分出发的预演结果：{{
            policyPreview.toFixed(3)
          }}。本次预演不改实际分数。
        </p>
        <label class="checkbox"
          ><input
            v-model="policyForm.confirmed"
            type="checkbox"
          />明确确认新规则从生效水位向后应用</label
        ><button :disabled="busy" type="submit">保存好感规则</button>
      </form>
    </details>
    <p v-if="loading" role="status">正在加载人物与关系…</p>
    <div v-else-if="!people.length" class="empty-state">
      尚无人物档案。建立人物并审核资料后，已核验身份的相关信息才会用于对话。
    </div>
    <div v-else class="relationship-columns">
      <aside class="card relationship-list" aria-label="人物列表">
        <button
          v-for="p in people"
          :key="p.id"
          type="button"
          :class="{ active: selected?.id === p.id }"
          :disabled="busy"
          @click="choose(p)"
        >
          <strong>{{ p.display_name }}</strong
          ><span
            >{{ p.visibility === "shared" ? "已发布共享" : "来源私有"
            }}<template v-if="p.merged_into"> · 已合并</template></span
          ><small>{{ p.id.slice(0, 8) }} · v{{ p.revision }}</small>
        </button>
        <p class="muted">
          {{ offset + 1 }}–{{ Math.min(offset + people.length, total) }} /
          {{ total }}
        </p>
        <div class="actions">
          <button
            type="button"
            :disabled="offset === 0 || busy"
            @click="
              offset -= 50;
              load();
            "
          >
            上一页</button
          ><button
            type="button"
            :disabled="offset + 50 >= total || busy"
            @click="
              offset += 50;
              load();
            "
          >
            下一页
          </button>
        </div>
      </aside>
      <article v-if="selected" class="card relationship-detail">
        <div class="section-heading">
          <div>
            <h3>{{ selected.display_name }}</h3>
            <p class="muted">
              {{ conversationName(selected.source_conversation_id) }} · v{{
                selected.revision
              }}
            </p>
          </div>
          <button
            type="button"
            class="danger"
            :disabled="busy"
            @click="forget(selected, 'person')"
          >
            遗忘人物
          </button>
        </div>
        <p v-if="selected.merged_into" class="notice">
          该档案已合并到
          {{
            name(selected.merged_into)
          }}。原始资料保留归属；新候选请在主档案提交。
        </p>
        <form class="relationship-form" @submit.prevent="rename">
          <details>
            <summary>更正称呼</summary>
            <label>新称呼<input v-model="renameForm.name" required /></label
            ><label
              >更正依据<textarea v-model="renameForm.reason" required /></label
            ><button :disabled="busy" type="submit">保存称呼</button>
          </details>
        </form>
        <h4>人物资料</h4>
        <p v-if="!selected.assertions.length" class="muted">
          资料未知，尚无字段断言。
        </p>
        <ul class="relationship-assertions">
          <li v-for="a in selected.assertions" :key="a.id">
            <div>
              <strong>{{ fieldNames[a.field] || a.field }}：</strong
              >{{ display(a.value) }}
              <span class="tag">{{ statuses[a.status] || a.status }}</span>
            </div>
            <p>
              {{ statements[a.statement_kind] || a.statement_kind }} ·
              {{ a.source_type }} / {{ a.source_id }} v{{ a.source_revision }}
            </p>
            <p>{{ a.reason }}</p>
            <small v-if="a.person_id !== selected.id"
              >来自合并前档案 {{ a.person_id }}</small
            >
            <div class="actions">
              <template v-if="a.status === 'pending'"
                ><button
                  type="button"
                  :disabled="busy"
                  @click="review(a, 'approve')"
                >
                  批准资料</button
                ><button
                  type="button"
                  :disabled="busy"
                  @click="review(a, 'reject')"
                >
                  驳回资料
                </button></template
              ><button
                type="button"
                :disabled="busy"
                @click="forget(a, 'assertion')"
              >
                遗忘字段
              </button>
            </div>
          </li>
        </ul>
        <label
          >审核理由<textarea
            v-model="reviewReason"
            placeholder="审核资料或关系前填写"
          />
        </label>
        <fieldset v-if="currentAssertions.length">
          <legend>本次明确替代的旧字段</legend>
          <label v-for="a in currentAssertions" :key="a.id" class="checkbox"
            ><input v-model="supersedes" type="checkbox" :value="a.id" />{{
              fieldNames[a.field]
            }}：{{ display(a.value) }}</label
          >
        </fieldset>
        <details v-if="!selected.merged_into">
          <summary>提交资料候选</summary>
          <form class="relationship-form" @submit.prevent="propose">
            <label
              >字段<AdminSelect
                v-model="assertionForm.field"
                :options="
                  Object.entries(fieldNames).map(([value, label]) => ({
                    value,
                    label,
                  }))
                " /></label
            ><label
              >字段内容<textarea v-model="assertionForm.value" required />
            </label>
            <p v-if="assertionForm.field === 'age'" class="muted">
              示例：{"years":21,"stated_at":"2026-10-26"}
            </p>
            <p v-if="assertionForm.field === 'birthday'" class="muted">
              示例：{"calendar":"gregorian","precision":"month_day","month":7,"day":12}。不由年龄推测出生日期。
            </p>
            <label
              >陈述类型<AdminSelect
                v-model="assertionForm.statement"
                :options="
                  Object.entries(statements).map(([value, label]) => ({
                    value,
                    label,
                  }))
                " /></label
            ><label
              >来源类型<AdminSelect
                v-model="assertionForm.source_type"
                :options="[
                  { value: 'manual', label: '人工记录' },
                  { value: 'diary_event', label: '日记事件' },
                  { value: 'diary_entry', label: '日记条目' },
                  { value: 'message', label: '已完成对话用户消息' },
                ]" /></label
            ><template v-if="assertionForm.source_type !== 'manual'"
              ><label
                >来源 ID<input
                  v-model="assertionForm.source_id"
                  required /></label
              ><label
                >来源版本<input
                  v-model.number="assertionForm.source_revision"
                  type="number"
                  min="1"
                  required /></label></template
            ><label
              >来源与依据<textarea
                v-model="assertionForm.reason"
                required
              /></label
            ><button type="submit" :disabled="busy">提交候选</button>
          </form>
        </details>
        <details v-if="!selected.merged_into">
          <summary>从日记整理人物候选</summary>
          <p class="muted">
            只读取本次选中的一条日记事件；来源必须已开启日记收集与模型出域。生成结果全部待审核，不自动合并人物。
          </p>
          <button type="button" :disabled="busy" @click="loadModelSources">
            加载可选日记来源
          </button>
          <form class="relationship-form" @submit.prevent="generateCandidates">
            <label
              >本次唯一来源<AdminSelect
                v-model="modelForm.source"
                :options="[
                  { value: '', label: '请选择明确授权的日记事件' },
                  ...modelSources.map((e) => ({
                    value: e.id,
                    label: `${e.content.slice(0, 80)}（v${e.revision}）`,
                  })),
                ]"
            /></label>
            <fieldset v-if="compatiblePeople.length">
              <legend>允许出现的关系目标（可不选）</legend>
              <label v-for="p in compatiblePeople" :key="p.id" class="checkbox"
                ><input
                  v-model="modelForm.targets"
                  type="checkbox"
                  :value="p.id"
                />{{ p.display_name }}</label
              >
            </fieldset>
            <label class="checkbox"
              ><input
                v-model="modelForm.confirmed"
                type="checkbox"
              />明确允许选中来源发送给当前配置模型并生成待审候选</label
            ><button :disabled="busy" type="submit">生成待审人物候选</button>
          </form>
          <button type="button" :disabled="busy" @click="refreshModelJobs">
            刷新整理进度
          </button>
          <ul>
            <li v-for="job in modelJobs" :key="job.id">
              {{ job.id.slice(0, 8) }} · {{ modelStatus(job.status) }}
              <span v-if="job.error">（{{ job.error }}）</span
              ><span v-if="job.status === 'completed'">
                · 字段 {{ job.result.assertion_ids?.length || 0 }}，关系
                {{ job.result.edge_ids?.length || 0 }}；均须审核</span
              >
            </li>
          </ul>
        </details>
        <details>
          <summary>核验渠道身份</summary>
          <ul>
            <li v-for="identity in selected.identities" :key="identity.id">
              {{ identity.alias }} ·
              {{ conversationName(identity.conversation_id) }}
              <button
                type="button"
                :disabled="busy"
                @click="
                  perform(
                    () =>
                      api(
                        `${personPath(selected!.id)}/identities/${encodeURIComponent(identity.id)}`,
                        { method: 'DELETE', revision: selected!.revision },
                      ),
                    '身份绑定已解除。',
                  )
                "
              >
                解除绑定
              </button>
            </li>
          </ul>
          <form class="relationship-form" @submit.prevent="bind">
            <label
              >已存在的真实会话<AdminSelect
                v-model="identityForm.conversation_id"
                :options="[
                  { value: '', label: '请选择已核验会话' },
                  ...conversations.map((c) => ({
                    value: c.id,
                    label: c.title,
                  })),
                ]" /></label
            ><label
              >软件昵称<input v-model="identityForm.alias" required /></label
            ><label
              >核验依据<textarea
                v-model="identityForm.reason"
                required
              /></label
            ><label class="checkbox"
              ><input
                v-model="identityForm.confirmed"
                type="checkbox"
              />确认该渠道会话属于此人物</label
            ><button :disabled="busy" type="submit">绑定身份</button>
          </form>
        </details>
        <details v-if="!selected.merged_into">
          <summary>跨软件同人映射</summary>
          <p class="muted">
            QQ、微信等账号可在本人核验后归入同一身份组。映射不会转授其他来源的读取权，字段、关系与好感事件继续保留各自归属。
          </p>
          <ul v-if="selected.identity_group">
            <li
              v-for="member in selected.identity_group.members"
              :key="member.id"
            >
              {{ member.display_name }} ·
              {{ conversationName(member.source_conversation_id) }}
            </li>
          </ul>
          <form class="relationship-form" @submit.prevent="linkIdentityGroup">
            <label
              >另一个已核验的账号档案<AdminSelect
                v-model="groupForm.other"
                :options="[
                  { value: '', label: '请选择另一个来源的真实人物档案' },
                  ...activePeople
                    .filter((p) => p.id !== selected!.id)
                    .map((p) => ({
                      value: p.id,
                      label: `${p.display_name} · ${conversationName(p.source_conversation_id)}`,
                    })),
                ]" /></label
            ><label
              >两侧核验 / 拆分依据<textarea
                v-model="groupForm.reason"
                required
              /></label
            ><label class="checkbox"
              ><input
                v-model="groupForm.confirmed"
                type="checkbox"
              />已核对真实身份，明确确认本次映射变更</label
            >
            <div class="actions">
              <button :disabled="busy" type="submit">确认同人关联</button
              ><button
                v-if="selected.identity_group"
                type="button"
                :disabled="busy"
                @click="unlinkIdentityGroup"
              >
                解除此账号关联
              </button>
            </div>
          </form>
        </details>
        <details>
          <summary>
            {{ selected.merged_into ? "拆分身份映射" : "合并身份映射" }}
          </summary>
          <form
            class="relationship-form"
            @submit.prevent="selected!.merged_into ? split() : merge()"
          >
            <label v-if="!selected.merged_into"
              >同一来源内的目标人物<AdminSelect
                v-model="mergeForm.target"
                :options="[
                  { value: '', label: '请选择核验后的目标' },
                  ...compatiblePeople.map((p) => ({
                    value: p.id,
                    label: `${p.display_name}（${p.id.slice(0, 8)}）`,
                  })),
                ]" /></label
            ><label
              >身份依据<textarea v-model="mergeForm.reason" required /></label
            ><label class="checkbox"
              ><input
                v-model="mergeForm.confirmed"
                type="checkbox"
              />已核对身份，明确确认本次映射变更</label
            >
            <p class="muted">
              保留原始字段、关系端点与独立好感事件，不累加分数；拆分恢复来源归属。
            </p>
            <button :disabled="busy" type="submit">
              {{ selected.merged_into ? "确认拆分" : "确认合并" }}
            </button>
          </form>
        </details>
        <details v-if="!selected.merged_into">
          <summary>建立有向关系</summary>
          <form class="relationship-form" @submit.prevent="createEdge">
            <p>
              方向：{{ selected.display_name }} → 目标人物。反向关系需独立依据。
            </p>
            <label
              >目标人物<AdminSelect
                v-model="edgeForm.to"
                :options="[
                  { value: '', label: '请选择目标' },
                  ...compatiblePeople.map((p) => ({
                    value: p.id,
                    label: p.display_name,
                  })),
                ]" /></label
            ><label
              >关系类型<AdminSelect
                v-model="edgeForm.type"
                :options="
                  Object.entries(edgeNames).map(([value, label]) => ({
                    value,
                    label,
                  }))
                " /></label
            ><label
              >陈述类型<AdminSelect
                v-model="edgeForm.statement"
                :options="
                  Object.entries(statements).map(([value, label]) => ({
                    value,
                    label,
                  }))
                " /></label
            ><label
              >关系依据<textarea v-model="edgeForm.reason" required /></label
            ><button :disabled="busy" type="submit">提交关系候选</button>
          </form>
        </details>
        <details>
          <summary>好感与联系许可</summary>
          <p>
            当前好感 {{ selected.affinity.score.toFixed(3) }} / 100。宽限期
            {{ selected.affinity.grace_seconds / 86400 }} 天，之后每日衰减
            {{
              selected.affinity.daily_decay
            }}。按经过时间结算，查询不重复扣分。
          </p>
          <p class="muted">
            好感是有向互动状态，不能代替接收方许可。{{
              selected.contact_permission.blocked_reason
            }}
          </p>
          <p>
            联系许可：{{ selected.contact_permission.status }}；主动投递：{{
              selected.contact_permission.delivery_available ? "可用" : "阻断"
            }}
          </p>
          <button type="button" :disabled="busy" @click="revokeContact">
            撤销主动联系许可
          </button>
          <form class="relationship-form" @submit.prevent="adjustAffinity">
            <label
              >人工调整（单次 ±5，每日累计绝对值 ≤10）<input
                v-model.number="affinityForm.delta"
                type="number"
                min="-5"
                max="5"
                step="0.1"
                required /></label
            ><label
              >调整依据<textarea
                v-model="affinityForm.reason"
                required
              /></label
            ><button :disabled="busy" type="submit">保存好感事件</button>
          </form>
        </details>
      </article>
      <div v-else class="card empty-state">
        选择一个人物，查看字段来源、冲突审核、身份映射和好感变化。
      </div>
    </div>
    <section v-if="people.length" class="card relationship-graph">
      <h3>有向关系图</h3>
      <p class="muted">
        仅展示当前页前 20
        个未合并人物及已审核关系。间接了解不代表直接认识；箭头不代表双向。
      </p>
      <svg
        :viewBox="`0 0 760 ${graphHeight}`"
        role="img"
        aria-label="已审核的有向人物关系图"
      >
        <defs>
          <marker
            :id="`relationship-arrow-${uid}`"
            markerWidth="8"
            markerHeight="8"
            refX="6"
            refY="3"
            orient="auto"
          >
            <path d="M0,0 L0,6 L6,3 z" fill="currentColor" />
          </marker>
        </defs>
        <g
          v-for="edge in graphEdges"
          :key="edge.id"
          class="relationship-graph-edge"
        >
          <line
            :x1="positions.get(edge.from_person_id)!.x"
            :y1="positions.get(edge.from_person_id)!.y"
            :x2="positions.get(edge.to_person_id)!.x - 48"
            :y2="positions.get(edge.to_person_id)!.y"
            :marker-end="`url(#relationship-arrow-${uid})`"
          />
          <text
            :x="
              (positions.get(edge.from_person_id)!.x +
                positions.get(edge.to_person_id)!.x) /
              2
            "
            :y="
              (positions.get(edge.from_person_id)!.y +
                positions.get(edge.to_person_id)!.y) /
                2 -
              9
            "
          >
            {{ edgeNames[edge.relationship_type] }}
          </text>
        </g>
        <g
          v-for="p in graphPeople"
          :key="p.id"
          class="relationship-graph-node"
          :transform="`translate(${positions.get(p.id)!.x},${positions.get(p.id)!.y})`"
        >
          <rect x="-68" y="-22" width="136" height="44" rx="14" />
          <text text-anchor="middle" dominant-baseline="middle">
            {{ p.display_name.slice(0, 9) }}
          </text>
          <title>{{ p.display_name }} · {{ p.id }}</title>
        </g>
      </svg>
    </section>
    <section v-if="relationships.length" class="card">
      <h3>关系及候选列表</h3>
      <ul class="relationship-assertions">
        <li v-for="e in relationships" :key="e.id">
          <strong
            >{{ name(e.from_person_id) }} → {{ name(e.to_person_id) }} ·
            {{ edgeNames[e.relationship_type] }}</strong
          ><span class="tag">{{ statuses[e.status] }}</span>
          <p>{{ statements[e.statement_kind] }}：{{ e.reason }}</p>
          <div class="actions">
            <template v-if="e.status === 'pending'"
              ><button
                type="button"
                :disabled="busy"
                @click="reviewEdge(e, 'approve')"
              >
                批准关系</button
              ><button
                type="button"
                :disabled="busy"
                @click="reviewEdge(e, 'reject')"
              >
                驳回关系
              </button></template
            ><button type="button" :disabled="busy" @click="forget(e, 'edge')">
              遗忘关系
            </button>
          </div>
        </li>
      </ul>
    </section>
    <div
      v-if="deleteTarget"
      class="card relationship-forget"
      role="alertdialog"
      aria-label="确认遗忘"
    >
      <h3>
        确认遗忘{{
          deleteKind === "person"
            ? "人物"
            : deleteKind === "assertion"
              ? "字段"
              : "关系"
        }}
      </h3>
      <p>
        正文及旧版本内容将清除，相关模型上下文失效。原始消息与离线备份按各自保留流程处理。
      </p>
      <div class="actions">
        <button
          type="button"
          class="danger"
          :disabled="busy"
          @click="confirmDelete"
        >
          确认遗忘</button
        ><button type="button" :disabled="busy" @click="deleteTarget = null">
          取消
        </button>
      </div>
    </div>
  </section>
</template>

<style scoped>
.relationships-panel button {
  padding: 9px 14px;
  font-size: 13px;
}
.relationships-panel {
  display: grid;
  gap: 1.2rem;
}
.section-heading {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 1rem;
}
.section-heading h2,
.section-heading h3 {
  margin-top: 0;
}
.relationship-form {
  display: grid;
  gap: 0.8rem;
}
.relationship-search {
  display: flex;
  align-items: flex-end;
  gap: 0.6rem;
  flex-wrap: wrap;
}
label {
  display: grid;
  gap: 0.35rem;
}
input:not([type="checkbox"]),
textarea {
  width: 100%;
  box-sizing: border-box;
}
textarea {
  min-height: 70px;
}
.checkbox {
  display: flex;
  align-items: center;
  gap: 0.5rem;
}
.relationship-columns {
  display: grid;
  grid-template-columns: minmax(180px, 260px) minmax(0, 1fr);
  gap: 1rem;
}
.card {
  padding: 1.2rem;
  border: 1px solid var(--line);
  border-radius: 16px;
  background: var(--pearl);
}
.relationship-list {
  align-self: start;
  display: grid;
  gap: 0.5rem;
}
.relationship-list > button {
  display: grid;
  gap: 0.3rem;
  text-align: left;
  width: 100%;
  padding: 0.8rem;
}
.relationship-list > button.active {
  background: #f4f6fc;
  border-color: #8c9bc5;
  color: var(--indigo);
}
.relationship-list span,
.relationship-list small,
.muted {
  color: var(--muted, #696d66);
}
.relationship-detail {
  display: grid;
  gap: 0.9rem;
  align-content: start;
}
details {
  padding-block: 0.5rem;
  border-top: 1px solid var(--line);
}
summary {
  cursor: pointer;
  font-weight: 600;
  padding: 0.4rem 0;
}
.relationship-assertions {
  padding: 0;
  list-style: none;
  display: grid;
  gap: 0.8rem;
}
.relationship-assertions li {
  border: 1px solid var(--line);
  border-radius: 10px;
  padding: 0.8rem;
  overflow-wrap: anywhere;
}
.relationship-assertions p {
  font-size: 0.9rem;
  margin: 0.4rem 0;
}
.tag {
  display: inline-block;
  padding: 0.15rem 0.5rem;
  border-radius: 10px;
  background: var(--soft-blue);
  font-size: 0.8rem;
  margin-left: 0.4rem;
}
.actions {
  display: flex;
  gap: 0.5rem;
  flex-wrap: wrap;
}
.error {
  color: #9e3832;
}
.success {
  color: #3d663f;
}
.danger {
  color: #9e3832;
}
.relationship-graph svg {
  width: 100%;
  max-height: 640px;
  color: var(--indigo);
}
.relationship-graph-edge line {
  stroke: #8c9bc5;
  stroke-width: 2;
}
.relationship-graph-edge text {
  fill: var(--indigo);
  text-anchor: middle;
  font-size: 12px;
}
.relationship-graph-node rect {
  fill: var(--soft-blue);
  stroke: #8c9bc5;
}
.relationship-graph-node text {
  fill: var(--ink);
  font-size: 13px;
}
.relationship-forget {
  border-color: #c96e66;
  position: sticky;
  bottom: 1rem;
  box-shadow: 0 4px 24px #0002;
}
@media (max-width: 820px) {
  .relationship-columns {
    grid-template-columns: 1fr;
  }
  .relationship-list {
    max-height: 300px;
    overflow-y: auto;
  }
}
</style>
