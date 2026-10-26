<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, useId } from "vue";
import { api, errorMessage } from "../api";
import AdminSelect from "./AdminSelect.vue";
import Icon from "./Icon.vue";
import { layoutRelationshipGraph, graphEdgePath } from "../relationship-graph";
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
const graphPeople = computed(() => activePeople.value.slice(0, 50));
const graphEdges = computed(() => {
  const ids = new Set(graphPeople.value.map((person) => person.id));
  return relationships.value.filter(
    (edge) =>
      edge.status === "approved" &&
      ids.has(edge.from_person_id) &&
      ids.has(edge.to_person_id),
  );
});
const positions = computed(() =>
  layoutRelationshipGraph(graphPeople.value, graphEdges.value),
);
const selectedId = ref<string | null>(null);
const detailLoading = ref(false);
const zoom = ref(1);
const pan = reactive({ x: 0, y: 0 });
const graphSvg = ref<SVGSVGElement | null>(null);
const dragging = ref(false);
let dragStart: {
  pointer: number;
  x: number;
  y: number;
  panX: number;
  panY: number;
} | null = null;
const connectedIds = computed(() => {
  const ids = new Set<string>();
  if (selectedId.value) ids.add(selectedId.value);
  for (const edge of graphEdges.value) {
    if (edge.from_person_id === selectedId.value) ids.add(edge.to_person_id);
    if (edge.to_person_id === selectedId.value) ids.add(edge.from_person_id);
  }
  return ids;
});
const selectedEdges = computed(() =>
  relationships.value.filter(
    (edge) =>
      edge.from_person_id === selectedId.value ||
      edge.to_person_id === selectedId.value,
  ),
);
function degree(id: string) {
  return graphEdges.value.filter(
    (edge) => edge.from_person_id === id || edge.to_person_id === id,
  ).length;
}
function edgePath(edge: Edge) {
  return graphEdgePath(
    edge.id,
    positions.value.get(edge.from_person_id)!,
    positions.value.get(edge.to_person_id)!,
    graphEdges.value.some(
      (other) =>
        other.from_person_id === edge.to_person_id &&
        other.to_person_id === edge.from_person_id,
    ),
  );
}
function isConnected(edge: Edge) {
  return (
    edge.from_person_id === selectedId.value ||
    edge.to_person_id === selectedId.value
  );
}
function resetGraph() {
  zoom.value = 1;
  pan.x = 0;
  pan.y = 0;
}
function zoomGraph(amount: number) {
  zoom.value = Math.min(
    2.4,
    Math.max(0.5, Math.round((zoom.value + amount) * 100) / 100),
  );
}
function startPan(event: PointerEvent) {
  if (event.button !== 0) return;
  dragStart = {
    pointer: event.pointerId,
    x: event.clientX,
    y: event.clientY,
    panX: pan.x,
    panY: pan.y,
  };
  dragging.value = true;
  graphSvg.value?.setPointerCapture?.(event.pointerId);
}
function movePan(event: PointerEvent) {
  if (!dragStart || event.pointerId !== dragStart.pointer) return;
  const rect = graphSvg.value?.getBoundingClientRect();
  if (!rect || !rect.width || !rect.height) return;
  const ratio = Math.min(rect.width / 900, rect.height / 680);
  pan.x = Math.max(
    -900,
    Math.min(900, dragStart.panX + (event.clientX - dragStart.x) / ratio),
  );
  pan.y = Math.max(
    -680,
    Math.min(680, dragStart.panY + (event.clientY - dragStart.y) / ratio),
  );
}
function endPan() {
  dragStart = null;
  dragging.value = false;
}
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
    if (
      selectedId.value &&
      !p.items.some((person) => person.id === selectedId.value)
    ) {
      detailController?.abort();
      detailEpoch++;
      selectedId.value = null;
      selected.value = null;
      detailLoading.value = false;
    }
    resetGraph();
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
  selectedId.value = person.id;
  detailLoading.value = true;
  if (selected.value?.id !== person.id) selected.value = null;
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
  } finally {
    if (epoch === detailEpoch && !disposed) detailLoading.value = false;
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
        <h2>每一段关系，都有来处</h2>
        <p class="muted">
          从一个人出发，看见彼此的联系。资料与关系保留来源，同名不会自动合并。
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
    <p v-if="loading" role="status">正在加载人物与关系…</p>
    <div v-else-if="!people.length" class="empty-state">
      尚无人物档案。建立人物并审核资料后，已核验身份的相关信息才会用于对话。
    </div>
    <div v-else class="relationship-columns relationship-workspace">
      <aside class="relationship-list" aria-label="人物列表">
        <div class="relationship-list-heading">
          <h3>人物</h3>
          <span>{{ total }} 位</span>
        </div>
        <button
          v-for="p in people"
          :key="p.id"
          type="button"
          :class="{ active: selectedId === p.id }"
          :aria-pressed="selectedId === p.id"
          :disabled="busy"
          @click="choose(p)"
        >
          <span class="person-list-title"
            ><strong>{{ p.display_name }}</strong
            ><span class="person-degree">{{ degree(p.id) }} 条关系</span></span
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
      <section class="relationship-graph" aria-label="人物关系网络">
        <div class="graph-heading">
          <div>
            <h3>关系网络</h3>
            <span class="muted"
              >{{ graphPeople.length }} 位人物 ·
              {{ graphEdges.length }} 条已审核关系</span
            >
          </div>
          <span class="graph-live-label"><i></i> 来源可追溯</span>
        </div>
        <div class="graph-canvas" :class="{ dragging }">
          <svg
            ref="graphSvg"
            viewBox="0 0 900 680"
            role="group"
            aria-label="已审核的有向人物关系图"
            @wheel.prevent="zoomGraph($event.deltaY < 0 ? 0.1 : -0.1)"
            @pointerdown="startPan"
            @pointermove="movePan"
            @pointerup="endPan"
            @pointercancel="endPan"
            @lostpointercapture="endPan"
          >
            <defs>
              <marker
                :id="`relationship-arrow-${uid}`"
                markerWidth="8"
                markerHeight="8"
                refX="7"
                refY="4"
                orient="auto"
                markerUnits="userSpaceOnUse"
              >
                <path
                  d="M1,1 L7,4 L1,7"
                  fill="none"
                  stroke="context-stroke"
                  stroke-width="1.3"
                />
              </marker>
            </defs>
            <g
              class="relationship-graph-viewport"
              :transform="`translate(${450 + pan.x} ${340 + pan.y}) scale(${zoom}) translate(-450 -340)`"
            >
              <g
                v-for="edge in graphEdges"
                :key="edge.id"
                class="relationship-graph-edge"
                :class="{
                  highlighted: isConnected(edge),
                  dimmed: selectedId && !isConnected(edge),
                }"
              >
                <path
                  :d="edgePath(edge)"
                  :marker-end="`url(#relationship-arrow-${uid})`"
                />
                <title>
                  {{ name(edge.from_person_id) }} →
                  {{ name(edge.to_person_id) }} ·
                  {{
                    edgeNames[edge.relationship_type] || edge.relationship_type
                  }}
                </title>
                <text
                  v-if="isConnected(edge)"
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
                  {{
                    edgeNames[edge.relationship_type] || edge.relationship_type
                  }}
                </text>
              </g>
              <g
                v-for="person in graphPeople"
                :key="person.id"
                class="relationship-graph-node"
                :class="{
                  selected: selectedId === person.id,
                  neighbor: selectedId && connectedIds.has(person.id),
                  dimmed: selectedId && !connectedIds.has(person.id),
                }"
                :transform="`translate(${positions.get(person.id)!.x},${positions.get(person.id)!.y})`"
                role="button"
                tabindex="0"
                :aria-label="`查看${person.display_name}的资料`"
                :aria-pressed="selectedId === person.id"
                :aria-disabled="busy"
                @pointerdown.stop
                @click.stop="!busy && choose(person)"
                @keydown.enter.prevent="!busy && choose(person)"
                @keydown.space.prevent="!busy && choose(person)"
              >
                <circle
                  class="node-halo"
                  :r="positions.get(person.id)!.radius + 6"
                />
                <circle
                  class="node-disc"
                  :r="positions.get(person.id)!.radius"
                  :style="{
                    '--node-opacity': String(
                      0.36 +
                        Math.min(positions.get(person.id)!.degree, 7) * 0.08,
                    ),
                  }"
                />
                <text text-anchor="middle" dominant-baseline="middle">
                  {{
                    person.display_name.length > 8
                      ? person.display_name.slice(0, 7) + "…"
                      : person.display_name
                  }}
                </text>
                <title>
                  {{ person.display_name }} ·
                  {{ degree(person.id) }} 条已审核关系 · {{ person.id }}
                </title>
              </g>
            </g>
          </svg>
          <div v-if="!graphPeople.length" class="graph-empty">
            当前页没有未合并人物。
          </div>
          <div class="graph-controls" role="group" aria-label="关系图视图控制">
            <button
              type="button"
              aria-label="缩小关系图"
              :disabled="zoom <= 0.5"
              @click="zoomGraph(-0.2)"
            >
              −
            </button>
            <output aria-label="关系图缩放比例"
              >{{ Math.round(zoom * 100) }}%</output
            >
            <button
              type="button"
              aria-label="放大关系图"
              :disabled="zoom >= 2.4"
              @click="zoomGraph(0.2)"
            >
              +
            </button>
            <span class="control-divider"></span
            ><button type="button" aria-label="重置关系图" @click="resetGraph">
              <Icon name="refresh" :size="16" />
            </button>
          </div>
          <p class="graph-hint">滚轮缩放 · 拖动空白移动 · 点击人物查看</p>
        </div>
        <div class="graph-legend">
          <span
            ><i class="legend-small"></i
            ><i class="legend-large"></i>圆点越大，当前页关联越多</span
          ><span><i class="legend-line"></i>已审核的有向关系</span>
        </div>
        <p class="graph-scope">
          展示当前页最多 50
          位未合并人物。箭头表示单向关系；间接联系不代表直接认识。
        </p>
      </section>
      <article
        v-if="selected && !detailLoading"
        class="relationship-detail"
        aria-label="人物详情"
      >
        <p class="eyebrow">PERSON PROFILE</p>
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
        <section class="person-connections" aria-label="当前人物的关系">
          <h4>
            与其他人的联系
            <span class="tag">{{
              selectedEdges.filter((edge) => edge.status === "approved").length
            }}</span>
          </h4>
          <p
            v-if="!selectedEdges.some((edge) => edge.status === 'approved')"
            class="muted"
          >
            还没有已审核关系。可以在下方提交关系候选。
          </p>
          <ul v-else>
            <li
              v-for="edge in selectedEdges.filter(
                (item) => item.status === 'approved',
              )"
              :key="edge.id"
            >
              <span
                >{{ name(edge.from_person_id) }} →
                {{ name(edge.to_person_id) }}</span
              ><strong>{{
                edgeNames[edge.relationship_type] || edge.relationship_type
              }}</strong>
            </li>
          </ul>
        </section>
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
      <aside
        v-else
        class="relationship-detail detail-placeholder"
        aria-label="人物详情"
      >
        <span class="detail-placeholder-icon"
          ><Icon name="channel" :size="28"
        /></span>
        <h3>{{ detailLoading ? "正在载入人物…" : "从一个人开始了解" }}</h3>
        <p>
          {{
            detailLoading
              ? "正在读取资料与来源。"
              : "点击关系图中的圆点，或从左侧选择人物。"
          }}
        </p>
        <p class="muted">在这里查看人物资料、关系来源与审核记录。</p>
      </aside>
    </div>
    <details v-if="relationships.length" class="card relationship-review-list">
      <summary>
        关系与候选审核 ·
        {{ relationships.filter((edge) => edge.status === "pending").length }}
        条待审核
      </summary>
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
    </details>
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
.relationships-panel {
  display: grid;
  gap: 1.2rem;
}
.relationships-panel button {
  padding: 9px 14px;
  font-size: 13px;
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
.relationship-search label {
  flex: 1;
  max-width: 400px;
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
  grid-template-columns: 190px minmax(0, 1fr) 310px;
  border: 1px solid var(--line);
  border-radius: 18px;
  background: var(--pearl);
  overflow: hidden;
  height: clamp(560px, calc(100dvh - 320px), 780px);
  min-height: 0;
}
.relationship-columns > * {
  min-width: 0;
}
.card {
  padding: 1.2rem;
  border: 1px solid var(--line);
  border-radius: 16px;
  background: var(--pearl);
}
.relationship-list {
  min-width: 0;
  max-height: 100%;
  overflow: auto;
  align-self: stretch;
  padding: 18px 0;
  border-right: 1px solid var(--line);
}
.relationship-list-heading {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 0 16px 12px;
}
.relationship-list-heading h3 {
  margin: 0;
  font-size: 14px;
}
.relationship-list-heading > span {
  font-size: 12px;
  color: var(--muted);
}
.relationship-list > button {
  display: grid;
  gap: 7px;
  text-align: left;
  width: 100%;
  padding: 16px;
  border: 0;
  border-bottom: 1px solid var(--line);
  border-radius: 0;
  background: transparent;
  color: var(--ink);
}
.relationship-list > button:hover {
  background: #f4f7fa;
}
.relationship-list > button.active {
  background: #edf5f5;
  box-shadow: inset 3px 0 #418386;
  color: #24595e;
}
.person-list-title {
  display: flex;
  justify-content: space-between;
  gap: 5px;
  align-items: baseline;
}
.person-list-title strong {
  font-size: 13px;
  overflow-wrap: anywhere;
}
.person-degree {
  font-size: 10px;
  white-space: nowrap;
}
.relationship-list > button > span:not(.person-list-title),
.relationship-list small,
.muted {
  color: var(--muted, #696d66);
}
.relationship-list > button > span:not(.person-list-title) {
  font-size: 11px;
}
.relationship-list small {
  font-size: 10px;
}
.relationship-list > p,
.relationship-list > .actions {
  margin: 15px 12px 0;
  font-size: 11px;
}
.relationship-list > .actions {
  gap: 5px;
}
.relationship-list > .actions button {
  padding: 7px;
  font-size: 11px;
}
.relationship-detail {
  padding: 22px;
  border-left: 1px solid var(--line);
  display: grid;
  gap: 0.9rem;
  align-content: start;
  max-height: 100%;
  overflow: auto;
  overflow-wrap: anywhere;
}
.relationship-detail .section-heading {
  gap: 7px;
}
.relationship-detail .section-heading h3 {
  font-size: 21px;
}
.relationship-detail .section-heading .danger {
  font-size: 11px;
  padding: 6px;
  white-space: nowrap;
}
.relationship-detail p {
  margin: 0;
  font-size: 12px;
  line-height: 1.7;
}
.relationship-detail h4 {
  margin: 8px 0 0;
  font-size: 14px;
}
.relationship-detail .eyebrow {
  color: #6c8f94;
  font-size: 10px;
  letter-spacing: 0.14em;
}
.relationship-detail label {
  font-size: 12px;
}
.person-connections {
  border-block: 1px solid var(--line);
  padding: 6px 0 14px;
}
.person-connections h4 {
  margin-bottom: 12px;
}
.person-connections ul {
  display: grid;
  gap: 10px;
  list-style: none;
  padding: 0;
  margin: 0;
}
.person-connections li {
  display: flex;
  justify-content: space-between;
  gap: 8px;
  font-size: 12px;
}
.person-connections li strong {
  color: #367a7b;
  white-space: nowrap;
  font-weight: 500;
}
.detail-placeholder {
  text-align: center;
  padding: 100px 24px 30px;
}
.detail-placeholder h3 {
  font-size: 17px;
  margin: 4px 0;
}
.detail-placeholder-icon {
  display: grid;
  place-items: center;
  width: 64px;
  height: 64px;
  border-radius: 50%;
  margin: 0 auto 14px;
  color: #6f9c9c;
  background: #edf5f5;
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
.error,
.danger {
  color: #9e3832;
}
.success {
  color: #3d663f;
}
.relationship-graph {
  min-width: 0;
  display: flex;
  flex-direction: column;
  background: #fdfefe;
}
.graph-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 20px 22px 0;
  gap: 12px;
}
.graph-heading h3 {
  font-size: 15px;
  margin: 0 0 5px;
}
.graph-heading .muted {
  font-size: 11px;
}
.graph-live-label {
  font-size: 10px;
  color: #548587;
  white-space: nowrap;
}
.graph-live-label i {
  display: inline-block;
  width: 5px;
  height: 5px;
  margin-right: 5px;
  border-radius: 50%;
  background: #66a1a1;
}
.graph-canvas {
  position: relative;
  min-height: 480px;
  flex: 1;
}
.graph-canvas > svg {
  width: 100%;
  height: 100%;
  min-height: 480px;
  display: block;
  position: absolute;
  inset: 0;
  touch-action: none;
  cursor: grab;
  user-select: none;
}
.graph-canvas.dragging > svg {
  cursor: grabbing;
}
.relationship-graph-edge path {
  stroke: #88a4a6;
  stroke-width: 1.2;
  fill: none;
  opacity: 0.62;
}
.relationship-graph-edge.highlighted path {
  stroke: #367b7e;
  stroke-width: 2;
  opacity: 0.95;
}
.relationship-graph-edge.dimmed {
  opacity: 0.15;
}
.relationship-graph-edge text {
  fill: #376d72;
  text-anchor: middle;
  font-size: 12px;
  paint-order: stroke;
  stroke: #fdfefe;
  stroke-width: 4px;
  stroke-linejoin: round;
}
.relationship-graph-node {
  cursor: pointer;
  outline: none;
}
.relationship-graph-node .node-disc {
  fill: #3c8083;
  fill-opacity: var(--node-opacity, 0.5);
  stroke: #fdfefe;
  stroke-width: 1.5;
}
.relationship-graph-node .node-halo {
  fill: none;
  stroke: transparent;
  stroke-width: 2.5;
}
.relationship-graph-node.selected .node-halo {
  stroke: #a36b9c;
}
.relationship-graph-node.selected .node-disc {
  fill-opacity: 0.92;
  stroke: #fdfefe;
  stroke-width: 2;
}
.relationship-graph-node.neighbor .node-disc {
  stroke: #7aa4a5;
}
.relationship-graph-node.dimmed {
  opacity: 0.28;
}
.relationship-graph-node:hover .node-halo,
.relationship-graph-node:focus-visible .node-halo {
  stroke: #448285;
  stroke-dasharray: 4 3;
}
.relationship-graph-node text {
  fill: #244e57;
  font-size: 13px;
  font-weight: 500;
  pointer-events: none;
  paint-order: stroke;
  stroke: #fdfefe;
  stroke-width: 2px;
  stroke-opacity: 0.9;
}
.relationship-graph-node.selected text {
  fill: white;
  stroke: #3c8083;
  stroke-opacity: 0.6;
}
.graph-controls {
  position: absolute;
  bottom: 18px;
  left: 18px;
  display: flex;
  align-items: center;
  gap: 2px;
  background: white;
  border: 1px solid #dce7e8;
  border-radius: 9px;
  padding: 3px;
  box-shadow: 0 3px 12px #2e666608;
}
.graph-controls button {
  width: 30px;
  height: 30px;
  border: 0;
  padding: 0;
  display: grid;
  place-items: center;
  background: transparent;
  font-size: 19px;
  color: #4c797d;
}
.graph-controls output {
  min-width: 40px;
  font-size: 11px;
  text-align: center;
  color: #52777c;
}
.control-divider {
  height: 16px;
  border-left: 1px solid #e4eded;
  margin: 0 3px;
}
.graph-hint {
  position: absolute;
  bottom: 23px;
  right: 18px;
  margin: 0;
  color: #91a2a6;
  font-size: 10px;
  pointer-events: none;
}
.graph-legend {
  padding: 16px 20px 10px;
  display: flex;
  gap: 16px;
  flex-wrap: wrap;
  border-top: 1px solid #edf2f2;
  color: #758c90;
  font-size: 10px;
}
.graph-legend span {
  display: flex;
  align-items: center;
  gap: 5px;
}
.legend-small,
.legend-large {
  display: inline-block;
  border-radius: 50%;
  background: #5d9395;
}
.legend-small {
  width: 7px;
  height: 7px;
  opacity: 0.4;
}
.legend-large {
  width: 12px;
  height: 12px;
  opacity: 0.7;
}
.legend-line {
  width: 20px;
  border-top: 1px solid #82a2a3;
}
.graph-scope {
  font-size: 10px;
  line-height: 1.7;
  color: #94a2a6;
  margin: 0;
  padding: 0 20px 16px;
}
.graph-empty {
  position: absolute;
  inset: 40% 20px auto;
  text-align: center;
  color: var(--muted);
  font-size: 13px;
}
.relationship-forget {
  border-color: #c96e66;
  position: sticky;
  bottom: 1rem;
  box-shadow: 0 4px 24px #0002;
}
@media (max-width: 1200px) {
  .relationship-columns {
    grid-template-columns: 170px minmax(0, 1fr);
    height: auto;
  }
  .relationship-detail {
    grid-column: 1 / -1;
    max-height: none;
    border-left: 0;
    border-top: 1px solid var(--line);
  }
  .detail-placeholder {
    padding: 30px 24px;
  }
  .graph-hint {
    display: none;
  }
}
@media (max-width: 720px) {
  .relationship-columns {
    grid-template-columns: 1fr;
  }
  .relationship-list {
    max-height: 220px;
    border-right: 0;
    border-bottom: 1px solid var(--line);
  }
  .graph-canvas,
  .graph-canvas > svg {
    min-height: 400px;
  }
  .graph-heading {
    padding: 18px 16px 0;
  }
  .graph-live-label {
    display: none;
  }
  .relationship-detail {
    padding: 20px 16px;
  }
  .relationship-search label {
    width: 100%;
  }
  .section-heading {
    flex-wrap: wrap;
  }
}
</style>
