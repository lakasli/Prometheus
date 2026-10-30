<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { api } from "../api";
import ProfileModelPanel from "./ProfileModelPanel.vue";
import AffinityLedgerPanel from "./AffinityLedgerPanel.vue";
import ProfileGraph from "./ProfileGraph.vue";
const props = defineProps<{ refreshToken?: unknown }>();
const emit = defineEmits<{ error: [unknown]; dirty: [boolean]; changed: [] }>();
const busy = ref(false),
  notice = ref(""),
  items = ref<any[]>([]),
  total = ref(0),
  offset = ref(0),
  query = ref(""),
  selected = ref<any>(null),
  selectedId = ref<string | null>(null),
  detailLoading = ref(false),
  mappingTarget = ref<any>(null),
  sources = ref<any[]>([]),
  conversations = ref<any[]>([]),
  graph = ref<any>(null),
  history = ref<any[]>([]);
const mode = ref(""),
  form = reactive({
    name: "",
    notes: "",
    reason: "",
    conversation: "",
    visibility: "private",
    field: "hobby",
    value: "",
    statement: "self_report",
    source: "manual",
    from: "",
    until: "",
    age: 26,
    stated: "",
    calendar: "gregorian",
    precision: "month_day",
    year: 2000,
    month: 1,
    day: 1,
    leap: false,
    other: "",
    relation: "",
    alias: "",
    contact: "pending_verification",
    evidence: "",
    expires: "",
    confirmed: false,
  });
const fields = [
  ["name", "姓名"],
  ["nickname", "昵称"],
  ["age", "年龄及陈述日期"],
  ["birthday", "生日"],
  ["job", "工作"],
  ["personality", "性格"],
  ["hobby", "爱好"],
  ["preference", "偏好"],
  ["commitment", "约定"],
];
const statements: Record<string, string> = {
  self_report: "本人陈述",
  third_party: "第三方转述",
  inference: "推测",
  confirmed: "已核实",
};
const statuses: Record<string, string> = {
  pending: "待审核",
  approved: "已批准",
  rejected: "已拒绝",
  superseded: "已替代",
  forgotten: "已遗忘",
  invalidated: "来源已失效",
};
const modelDirty = ref(false),
  affinityDirty = ref(false),
  modelFormGeneration = ref(0);
const draft = computed(
  () => !!mode.value || modelDirty.value || affinityDirty.value,
);
watch(draft, (v) => emit("dirty", v));
let disposed = false,
  detailEpoch = 0;
let detailController: AbortController | undefined,
  loadController: AbortController | undefined;
onUnmounted(() => {
  disposed = true;
  detailEpoch++;
  detailController?.abort();
  loadController?.abort();
  emit("dirty", false);
});
const compatiblePeople = computed(() =>
  items.value.filter(
    (p) =>
      p.id !== selected.value?.id &&
      p.can_manage &&
      !p.merged_into &&
      p.owner_id === selected.value?.owner_id &&
      p.visibility === selected.value?.visibility,
  ),
);
const sourceOptions = computed(() =>
  sources.value.filter(
    (s) =>
      selected.value &&
      (s.visibility === "shared" || s.owner_id === selected.value.owner_id) &&
      !(selected.value.visibility === "shared" && s.visibility !== "shared"),
  ),
);
function begin(next: string) {
  if (draft.value && !confirm("放弃未保存的人物修改？")) return;
  modelDirty.value = false;
  affinityDirty.value = false;
  modelFormGeneration.value++;
  mode.value = next;
  mappingTarget.value = null;
  Object.assign(form, {
    name: selected.value?.data.name ?? "",
    notes: selected.value?.data.notes ?? "",
    reason: "",
    value: "",
    source: "manual",
    other: "",
    conversation: "",
    alias: "",
    evidence: "",
    from: "",
    until: "",
    expires: "",
    confirmed: false,
  });
  notice.value = "";
  history.value = [];
}
async function act(action: () => Promise<void>) {
  if (busy.value || disposed) return;
  busy.value = true;
  try {
    await action();
  } catch (e) {
    if (!disposed && !(e instanceof Error && e.name === "AbortError"))
      emit("error", e);
  } finally {
    busy.value = false;
  }
}
async function load() {
  loadController?.abort();
  const controller = new AbortController();
  loadController = controller;
  const params = `offset=${offset.value}&q=${encodeURIComponent(query.value)}`;
  const g = await api<any>(
    `/relationships/graph?${params}`,
    "GET",
    undefined,
    undefined,
    controller.signal,
  );
  if (disposed || controller.signal.aborted || loadController !== controller)
    return false;
  items.value = g.page_items;
  total.value = g.total;
  graph.value = g;
  if (
    selectedId.value &&
    !g.page_items.some((p: any) => p.id === selectedId.value)
  )
    clearSelection();
  return true;
}
function clearDraft() {
  mode.value = "";
  modelDirty.value = false;
  affinityDirty.value = false;
  modelFormGeneration.value++;
  mappingTarget.value = null;
  history.value = [];
}
function clearSelection() {
  detailEpoch++;
  detailController?.abort();
  selectedId.value = null;
  selected.value = null;
  detailLoading.value = false;
  clearDraft();
}
async function readDetail(id: string) {
  detailController?.abort();
  const controller = new AbortController(),
    epoch = ++detailEpoch;
  detailController = controller;
  detailLoading.value = true;
  try {
    const p = await api<any>(
      `/relationships/profiles/${id}`,
      "GET",
      undefined,
      undefined,
      controller.signal,
    );
    if (
      disposed ||
      controller.signal.aborted ||
      epoch !== detailEpoch ||
      selectedId.value !== id ||
      !items.value.some((p) => p.id === id)
    )
      return;
    selected.value = p;
  } catch (e) {
    if (!disposed && !controller.signal.aborted && epoch === detailEpoch)
      emit("error", e);
  } finally {
    if (!disposed && epoch === detailEpoch) detailLoading.value = false;
  }
}
async function select(id: string) {
  if (busy.value || !items.value.some((p) => p.id === id)) return;
  if (draft.value && !confirm("放弃未保存的人物修改？")) return;
  clearDraft();
  selectedId.value = id;
  selected.value = null;
  await readDetail(id);
}
async function refresh() {
  if (!(await load())) return;
  if (selectedId.value) await readDetail(selectedId.value);
}
async function browse(page = offset.value) {
  if (busy.value) return;
  if (draft.value && !confirm("放弃未保存的人物修改？")) return;
  clearDraft();
  detailController?.abort();
  detailEpoch++;
  offset.value = page;
  await act(refresh);
}
watch(
  () => props.refreshToken,
  () => {
    if (!draft.value) void act(refresh);
  },
);
async function saved(p: any) {
  selected.value = p;
  selectedId.value = p.id;
  mode.value = "";
  notice.value = "已保存。";
  await load();
  emit("changed");
}
const iso = (value: string) =>
  value ? Math.floor(new Date(value).getTime() / 1000) : null;
function source() {
  if (form.source === "manual") return { type: "manual" };
  const s = sourceOptions.value.find((s) => s.key === form.source);
  if (!s) throw new Error("请选择当前可用来源");
  return { type: s.type, id: s.id, revision: s.revision };
}
function value() {
  if (form.field === "age") return { years: form.age, stated_at: form.stated };
  if (form.field === "birthday")
    return {
      calendar: form.calendar,
      precision: form.precision,
      ...(form.precision !== "month_day" ? { year: form.year } : {}),
      ...(form.precision !== "year"
        ? {
            month: form.month,
            day: form.day,
            ...(form.calendar === "lunar" ? { leap_month: form.leap } : {}),
          }
        : {}),
    };
  return form.value;
}
async function save() {
  await act(async () => {
    const p = selected.value;
    switch (mode.value) {
      case "merge": {
        const target = compatiblePeople.value.find((p) => p.id === form.other);
        if (!target || !form.confirmed)
          throw new Error("请选择同域档案并明确核对映射");
        await saved(
          await api(
            `/relationships/profiles/${p.id}/merge`,
            "POST",
            {
              into_id: target.id,
              expected_into_revision: target.revision,
              reason: form.reason,
              confirmed: form.confirmed,
            },
            p.revision,
          ),
        );
        break;
      }
      case "split": {
        if (!mappingTarget.value || !form.confirmed)
          throw new Error("请核对目标并明确确认拆分");
        await saved(
          await api(
            `/relationships/profiles/${p.id}/split`,
            "POST",
            {
              expected_into_revision: mappingTarget.value.revision,
              reason: form.reason,
              confirmed: form.confirmed,
            },
            p.revision,
          ),
        );
        break;
      }
      case "create":
        await saved(
          await api("/relationships/profiles", "POST", {
            name: form.name,
            notes: form.notes,
            visibility: form.visibility,
            source_conversation_id: form.conversation || undefined,
            reason: form.reason,
            confirmed: true,
          }),
        );
        break;
      case "rename":
        await saved(
          await api(
            `/relationships/profiles/${p.id}`,
            "PATCH",
            { name: form.name, notes: form.notes, reason: form.reason },
            p.revision,
          ),
        );
        break;
      case "field":
        await saved(
          await api(
            `/relationships/profiles/${p.id}/assertions`,
            "POST",
            {
              field: form.field,
              value: value(),
              statement_kind: form.statement,
              source: source(),
              reason: form.reason,
              valid_from: iso(form.from),
              valid_until: iso(form.until),
            },
            p.revision,
          ),
        );
        break;
      case "edge": {
        const other = items.value.find((i) => i.id === form.other);
        await saved(
          await api(
            "/relationships/edges",
            "POST",
            {
              from_id: p.id,
              to_id: other.id,
              expected_to_revision: other.revision,
              relationship_type: form.relation,
              statement_kind: form.statement,
              source: source(),
              reason: form.reason,
            },
            p.revision,
          ),
        );
        break;
      }
      case "bind":
        await saved(
          await api(
            `/relationships/profiles/${p.id}/bind`,
            "POST",
            {
              conversation_id: form.conversation,
              alias: form.alias,
              reason: form.reason,
              confirmed: true,
            },
            p.revision,
          ),
        );
        break;
      case "link": {
        const other = await api<any>(`/relationships/profiles/${form.other}`);
        await saved(
          await api(
            `/relationships/profiles/${p.id}/link`,
            "POST",
            {
              other_id: other.id,
              expected_other_revision: other.revision,
              expected_group_revision:
                p.group?.revision ?? other.group?.revision ?? 0,
              reason: form.reason,
              confirmed: true,
            },
            p.revision,
          ),
        );
        break;
      }
      case "contact":
        await saved(
          await api(
            `/relationships/profiles/${p.id}/contact`,
            "POST",
            {
              status: form.contact,
              conversation_id: form.conversation || null,
              evidence: form.evidence,
              expires_at: iso(form.expires),
            },
            p.contact.revision,
          ),
        );
        break;
    }
  });
}
async function review(a: any, kind: string, action: string) {
  const reason = prompt("请填写此次审核、替代或遗忘的依据");
  if (!reason) return;
  const old =
    action === "approve"
      ? kind === "assertions"
        ? selected.value.assertions.filter(
            (v: any) => v.status === "approved" && v.field === a.field,
          )
        : selected.value.edges.filter(
            (v: any) =>
              v.status === "approved" &&
              v.from_person_id === a.from_person_id &&
              v.to_person_id === a.to_person_id,
          )
      : [];
  if (
    !confirm(
      `${action === "approve" ? "批准" : action === "reject" ? "拒绝" : "遗忘"}此陈述？${old.length ? "将替代下列当前已批准陈述：" + old.map((o: any) => (o.value ? JSON.stringify(o.value) : o.relationship_type)).join("；") : ""} 推测与转述标签保持原样。`,
    )
  )
    return;
  await act(async () =>
    saved(
      await api(
        `/relationships/${kind}/${a.id}/review`,
        "POST",
        {
          action,
          reason,
          supersedes: old.map((v: any) => ({ id: v.id, revision: v.revision })),
        },
        a.revision,
      ),
    ),
  );
}
async function unbind(identity: any) {
  const reason = prompt("填写撤销身份绑定的依据");
  if (!reason) return;
  await act(async () =>
    saved(
      await api(
        `/relationships/profiles/${selected.value.id}/unbind`,
        "POST",
        {
          identity_id: identity.id,
          expected_identity_revision: identity.revision,
          reason,
        },
        selected.value.revision,
      ),
    ),
  );
}
async function unlink() {
  const reason = prompt("填写拆分身份关联的依据");
  if (!reason || !confirm("拆分身份组？各档案资料、来源与分值继续独立。"))
    return;
  await act(async () =>
    saved(
      await api(
        `/relationships/profiles/${selected.value.id}/unlink`,
        "POST",
        {
          expected_group_revision: selected.value.group.revision,
          reason,
          confirmed: true,
        },
        selected.value.revision,
      ),
    ),
  );
}
async function prepareSplit() {
  if (busy.value || !selected.value?.merged_into) return;
  if (draft.value && !confirm("放弃未保存的人物修改？")) return;
  await act(async () => {
    const target = await api<any>(
      `/relationships/profiles/${selected.value.merged_into}`,
    );
    if (!target.can_manage) throw new Error("拆分须同时具有两个档案的管理权限");
    clearDraft();
    begin("split");
    mappingTarget.value = target;
  });
}
async function forget() {
  if (
    !confirm("遗忘此人物及其字段、关系、身份和联系证据？原聊天与离线备份保留。")
  )
    return;
  await act(async () => {
    await api(
      `/relationships/profiles/${selected.value.id}`,
      "DELETE",
      undefined,
      selected.value.revision,
    );
    clearSelection();
    await load();
  });
}
async function showHistory(a: any, kind: string) {
  await act(async () => {
    history.value = (
      await api<any>(`/relationships/${kind}/${a.id}/history`)
    ).items;
  });
}
onMounted(() =>
  act(async () => {
    await load();
    const [c, e, d, m] = await Promise.all([
      api<any>("/knowledge/sources"),
      api<any>("/diary/events?limit=100"),
      api<any>("/diary/entries?limit=100"),
      api<any>("/records/memory?limit=100"),
    ]);
    conversations.value = c.items;
    sources.value = [
      ...e.items.map((s: any) => ({
        ...s,
        type: "diary_event",
        label: s.content?.slice(0, 45) ?? "事件",
      })),
      ...d.items.map((s: any) => ({
        ...s,
        type: "diary_entry",
        label: `${s.period_start} ${s.period}`,
      })),
      ...m.items.map((s: any) => ({
        ...s,
        type: "memory",
        label: s.data.content.slice(0, 45),
      })),
    ].map((s) => ({ ...s, key: `${s.type}:${s.id}` }));
  }),
);
</script>
<template>
  <section class="panel profile-panel" aria-label="人物资料与身份">
    <h2>人物资料与身份</h2>
    <p>
      字段保留来源和陈述类型，人工批准后才可进入已核对身份的对话。身份关联不合并私有范围或好感分。
    </p>
    <p role="status" v-if="notice">{{ notice }}</p>
    <form class="profile-tools" @submit.prevent="browse(0)">
      <label>查找人物<input v-model="query" maxlength="128" /></label
      ><button :disabled="busy">查找</button
      ><button type="button" @click="begin('create')">建立来源域人物</button>
      <button type="button" :disabled="busy || !!mode" @click="act(refresh)">
        刷新人物资料
      </button>
    </form>
    <div class="profile-columns">
      <nav aria-label="人物档案">
        <button
          v-for="p in items"
          :key="p.id"
          :aria-pressed="selectedId === p.id"
          :disabled="busy"
          @click="select(p.id)"
        >
          {{ p.data.name }}
          <small
            >{{
              p.visibility === "shared"
                ? "共享"
                : p.owner_id === p.manager_id
                  ? "本人私有"
                  : "来源私有"
            }}{{ p.merged_into ? " · 已合并" : "" }}</small
          >
        </button>
        <p v-if="!items.length">暂无人物</p>
        <p>
          {{ offset + 1 }}–{{ Math.min(offset + items.length, total) }} /
          {{ total }}
        </p>
        <button :disabled="offset === 0 || busy" @click="browse(offset - 50)">
          上一页</button
        ><button
          :disabled="offset + 50 >= total || busy"
          @click="browse(offset + 50)"
        >
          下一页
        </button>
      </nav>
      <ProfileGraph
        v-if="graph"
        :items="graph.items"
        :edges="graph.edges"
        :selected-id="selectedId"
        :busy="busy"
        @select="select"
      />
      <div class="profile-detail">
        <p v-if="detailLoading" role="status">正在加载人物资料…</p>
        <template v-if="selected"
          ><h3>{{ selected.data.name }}</h3>
          <p>{{ selected.data.notes }}</p>
          <p v-if="selected.merged_into">
            此档案已映射到
            {{ selected.merged_into }}。原资料、关系端点和好感分继续独立。
          </p>
          <div v-if="selected.can_manage" class="profile-tools">
            <button @click="begin('rename')">更正名称与备注</button
            ><button v-if="!selected.merged_into" @click="begin('field')">
              新增字段陈述</button
            ><button @click="begin('edge')">新增有向关系</button
            ><button @click="begin('bind')">核对账号身份</button
            ><button @click="begin('link')">关联跨账号身份</button
            ><button @click="begin('contact')">联系许可记录</button
            ><button v-if="!selected.merged_into" @click="begin('merge')">
              合并同域档案
            </button>
            <button v-else @click="prepareSplit">拆分同域映射</button>
            <button class="danger" @click="forget">遗忘人物</button>
          </div>
          <h4>字段陈述</h4>
          <p v-if="!selected.assertions.length">
            暂无字段陈述，人工备注不自动转成已核实资料。
          </p>
          <article
            v-for="a in selected.assertions"
            :key="a.id"
            class="profile-statement"
          >
            <strong
              >{{ fields.find((f) => f[0] === a.field)?.[1] }} ·
              {{ statuses[a.status] }} ·
              {{ statements[a.statement_kind] }}</strong
            >
            <pre>{{
              typeof a.value === "string"
                ? a.value
                : JSON.stringify(a.value, null, 2)
            }}</pre>
            <small
              >{{ a.reason }} ·
              {{
                a.source.type === "manual"
                  ? "人工记录"
                  : a.source.type === "diary_event"
                    ? "日记事件"
                    : a.source.type === "diary_entry"
                      ? "日记条目"
                      : a.source.type === "message"
                        ? "用户原文"
                        : "记忆"
              }}{{ a.expired ? " · 已到期" : ""
              }}{{ a.not_started ? " · 尚未生效" : "" }}</small
            >
            <div class="profile-tools">
              <button
                v-if="selected.can_manage && a.status === 'pending'"
                @click="review(a, 'assertions', 'approve')"
              >
                审核批准</button
              ><button
                v-if="selected.can_manage && a.status === 'pending'"
                @click="review(a, 'assertions', 'reject')"
              >
                拒绝</button
              ><button @click="showHistory(a, 'assertions')">版本记录</button
              ><button
                v-if="selected.can_manage"
                @click="review(a, 'assertions', 'forget')"
              >
                遗忘字段
              </button>
            </div>
          </article>
          <h4>有向关系</h4>
          <article
            v-for="e in selected.edges"
            :key="e.id"
            class="profile-statement"
          >
            <p>
              {{
                items.find((p) => p.id === e.from_person_id)?.data.name ??
                "本页外人物"
              }}
              →
              {{
                items.find((p) => p.id === e.to_person_id)?.data.name ??
                "本页外人物"
              }}：{{ e.relationship_type }} ·
              {{ statements[e.statement_kind] }} · {{ statuses[e.status] }}
            </p>
            <div class="profile-tools">
              <button
                v-if="selected.can_manage && e.status === 'pending'"
                @click="review(e, 'edges', 'approve')"
              >
                审核批准</button
              ><button
                v-if="selected.can_manage && e.status === 'pending'"
                @click="review(e, 'edges', 'reject')"
              >
                拒绝</button
              ><button @click="showHistory(e, 'edges')">版本记录</button
              ><button
                v-if="selected.can_manage"
                @click="review(e, 'edges', 'forget')"
              >
                遗忘关系
              </button>
            </div>
          </article>
          <h4>已核对的账号</h4>
          <p v-if="!selected.identities.length">
            尚未绑定，对话不按同名自动认人。
          </p>
          <p v-for="i in selected.identities" :key="i.id">
            {{ i.alias || "已核对账号" }} ·
            {{ i.revoked_at ? "已撤销" : "有效" }}
            <button v-if="!i.revoked_at" @click="unbind(i)">撤销绑定</button>
          </p>
          <p v-if="selected.group">
            身份关联：{{
              selected.group.members.map((p: any) => p.data.name).join("、")
            }}
            <button v-if="selected.can_manage" @click="unlink">拆分关联</button>
          </p>
          <p>
            联系状态：{{
              selected.contact.status === "pending_verification"
                ? "待验证"
                : selected.contact.status === "revoked"
                  ? "已撤销"
                  : "未授予"
            }}。主动发送尚不可用。
          </p>
          <details>
            <summary>名称版本</summary>
            <p v-for="n in selected.name_history" :key="n.revision">
              {{ n.name }} · {{ n.reason || "原有人工档案" }} · 版本
              {{ n.revision }}
            </p>
          </details>
          <AffinityLedgerPanel
            v-if="selected.can_manage"
            :key="`ledger:${selected.id}:${modelFormGeneration}`"
            :person="selected"
            :sources="sourceOptions"
            @error="emit('error', $event)"
            @dirty="affinityDirty = $event"
            @changed="
              async () => {
                if (!mode) await refresh();
                emit('changed');
              }
            "
          />
          <ProfileModelPanel
            v-if="selected.can_manage"
            :key="`${selected.id}:${modelFormGeneration}`"
            :person="selected"
            :sources="sourceOptions"
            :targets="items"
            @error="emit('error', $event)"
            @dirty="modelDirty = $event"
            @changed="
              async () => {
                if (!mode) await refresh();
                emit('changed');
              }
            "
          />
        </template>
        <p v-else>选择人物查看来源与审核记录。</p>
        <form v-if="mode" class="profile-form" @submit.prevent="save">
          <h3>
            {{
              mode === "create"
                ? "建立人物"
                : mode === "field"
                  ? "字段陈述"
                  : mode === "edge"
                    ? "有向关系"
                    : mode === "bind"
                      ? "身份核对"
                      : mode === "link"
                        ? "跨账号关联"
                        : mode === "contact"
                          ? "联系许可"
                          : mode === "merge"
                            ? "合并同域档案"
                            : mode === "split"
                              ? "拆分同域映射"
                              : "名称与备注"
            }}
          </h3>
          <template v-if="mode === 'merge' || mode === 'split'">
            <p>
              只建立或拆开已核对的同域映射；原字段、关系端点、账号身份、好感分和联系许可保持各自归属。身份组须先解除，不能建立链式映射。
            </p>
            <label v-if="mode === 'merge'"
              >同域目标人物<select
                v-model="form.other"
                aria-label="同域目标人物"
                required
              >
                <option value="">请选择并核对目标</option>
                <option v-for="p in compatiblePeople" :key="p.id" :value="p.id">
                  {{ p.data.name }} · {{ p.id.slice(0, 8) }} · 版本{{
                    p.revision
                  }}
                </option>
              </select></label
            >
            <p v-else>
              原目标：{{ mappingTarget?.data.name }} · {{ mappingTarget?.id }} ·
              版本{{ mappingTarget?.revision }}
            </p>
            <label
              ><input
                v-model="form.confirmed"
                type="checkbox"
                required
              />已分别核对两个人物，明确确认本次映射变更</label
            >
          </template>
          <template v-if="['create', 'rename'].includes(mode)"
            ><label
              >名称<input v-model="form.name" required maxlength="128" /></label
            ><label
              >人工备注<textarea v-model="form.notes" maxlength="8000" /></label
          ></template>
          <template v-if="mode === 'create'"
            ><label
              >来源域<select v-model="form.conversation">
                <option value="">当前管理账户</option>
                <option v-for="c in conversations" :key="c.id" :value="c.id">
                  {{ c.title }} · {{ c.owner_id }}
                </option>
              </select></label
            ><label
              >范围<select v-model="form.visibility">
                <option value="private">私有</option>
                <option value="shared">明确共享</option>
              </select></label
            ></template
          >
          <template v-if="mode === 'field'"
            ><label
              >字段<select v-model="form.field">
                <option v-for="f in fields" :key="f[0]" :value="f[0]">
                  {{ f[1] }}
                </option>
              </select></label
            ><template v-if="form.field === 'age'"
              ><label
                >陈述年龄<input
                  type="number"
                  v-model.number="form.age"
                  min="0"
                  max="150"
                  required /></label
              ><label
                >陈述日期<input
                  type="date"
                  v-model="form.stated"
                  required /></label></template
            ><template v-else-if="form.field === 'birthday'"
              ><label
                >历法<select v-model="form.calendar">
                  <option value="gregorian">公历</option>
                  <option value="lunar">农历（不作换算）</option>
                </select></label
              ><label
                >已知精度<select v-model="form.precision">
                  <option value="year">仅年份</option>
                  <option value="month_day">仅月日</option>
                  <option value="date">完整日期</option>
                </select></label
              ><label v-if="form.precision !== 'month_day'"
                >年<input
                  type="number"
                  v-model.number="form.year"
                  min="1800"
                  max="2200" /></label
              ><template v-if="form.precision !== 'year'"
                ><label
                  >月<input
                    type="number"
                    v-model.number="form.month"
                    min="1"
                    max="12" /></label
                ><label
                  >日<input
                    type="number"
                    v-model.number="form.day"
                    min="1"
                    max="31" /></label
                ><label v-if="form.calendar === 'lunar'"
                  ><input type="checkbox" v-model="form.leap" /> 闰月</label
                ></template
              ></template
            ><label v-else
              >字段内容<textarea
                v-model="form.value"
                required
                maxlength="2000"
              /></label
            ><label
              >生效时间（可空）<input
                type="datetime-local"
                v-model="form.from" /></label
            ><label
              >失效时间（可空）<input
                type="datetime-local"
                v-model="form.until" /></label
          ></template>
          <template v-if="['field', 'edge'].includes(mode)"
            ><label
              >陈述类型<select v-model="form.statement" aria-label="陈述类型">
                <option v-for="(label, s) in statements" :key="s" :value="s">
                  {{ label }}
                </option>
              </select></label
            ><label
              >明确来源<select v-model="form.source" aria-label="明确来源">
                <option value="manual">人工记录</option>
                <option v-for="s in sourceOptions" :key="s.key" :value="s.key">
                  {{
                    s.type === "memory"
                      ? "记忆"
                      : s.type === "diary_event"
                        ? "事件"
                        : "日记"
                  }}：{{ s.label }}
                </option>
              </select></label
            ></template
          >
          <template v-if="['edge', 'link'].includes(mode)"
            ><label
              >{{ mode === "edge" ? "终点人物" : "另一个账号的人物"
              }}<select v-model="form.other" required>
                <option value="">请选择</option>
                <option
                  v-for="p in items.filter((p) => p.id !== selected?.id)"
                  :key="p.id"
                  :value="p.id"
                >
                  {{ p.data.name }} · {{ p.owner_id }}
                </option>
              </select></label
            ><label v-if="mode === 'edge'"
              >从当前人物指向对方的关系<input
                v-model="form.relation"
                required
                maxlength="200" /></label
          ></template>
          <template v-if="mode === 'bind'"
            ><label
              >本人私聊会话<select
                v-model="form.conversation"
                aria-label="本人私聊会话"
                required
              >
                <option value="">请选择并独立核对身份</option>
                <option
                  v-for="c in conversations.filter(
                    (c) => c.owner_id === selected?.owner_id,
                  )"
                  :key="c.id"
                  :value="c.id"
                >
                  {{ c.title }} · {{ c.owner_id }}
                </option>
              </select></label
            ><label
              >账号别名<input v-model="form.alias" maxlength="128" /></label
          ></template>
          <template v-if="mode === 'contact'"
            ><label
              >许可状态<select v-model="form.contact">
                <option value="not_granted">未授予</option>
                <option value="pending_verification">待验证</option>
                <option value="revoked">已撤销</option>
              </select></label
            ><template v-if="form.contact === 'pending_verification'"
              ><label
                >绑定会话<select v-model="form.conversation" required>
                  <option value="">请选择</option>
                  <option
                    v-for="i in selected.identities.filter(
                      (i: any) => !i.revoked_at,
                    )"
                    :key="i.id"
                    :value="i.conversation_id"
                  >
                    {{ i.alias || "已核对账号" }}
                  </option>
                </select></label
              ><label
                >许可证据<textarea
                  v-model="form.evidence"
                  required
                  maxlength="2000"
                /></label
              ><label
                >到期时间（可空）<input
                  type="datetime-local"
                  v-model="form.expires" /></label></template
          ></template>
          <label v-if="mode !== 'contact'"
            >依据<textarea v-model="form.reason" required maxlength="2000" />
          </label>
          <div class="profile-tools">
            <button :disabled="busy">
              {{
                mode === "field" || mode === "edge"
                  ? "保存待审核陈述"
                  : "核对并保存"
              }}</button
            ><button type="button" :disabled="busy" @click="mode = ''">
              取消
            </button>
          </div>
        </form>
        <details v-if="history.length" open>
          <summary>所选陈述的版本记录</summary>
          <article v-for="v in history" :key="v.revision">
            <p>
              版本 {{ v.revision }} · {{ statuses[v.status] }} · {{ v.reason }}
            </p>
            <pre>{{ v.value ?? v.relationship_type }}</pre>
          </article>
        </details>
      </div>
    </div>
  </section>
</template>
<style scoped>
.profile-panel {
  margin-top: 24px;
}
.profile-tools {
  display: flex;
  gap: 8px;
  align-items: end;
  flex-wrap: wrap;
  margin: 12px 0;
}
.profile-columns {
  display: grid;
  grid-template-columns: minmax(130px, 170px) minmax(230px, 1fr) minmax(
      280px,
      1.15fr
    );
  gap: 20px;
}
.profile-columns nav > button {
  display: block;
  max-width: 100%;
  margin: 8px 0;
  overflow-wrap: anywhere;
}
.profile-columns small {
  display: block;
}
.profile-detail {
  min-width: 0;
}
.profile-statement {
  border: 1px solid var(--border, #d9dce2);
  border-radius: 8px;
  padding: 12px;
  margin: 10px 0;
}
.profile-form {
  border: 1px solid var(--border, #d9dce2);
  padding: 16px;
  border-radius: 8px;
  margin: 18px 0;
}
.profile-form label {
  display: block;
  margin: 12px 0;
}
.profile-form input,
.profile-form textarea,
.profile-form select {
  display: block;
  max-width: 100%;
}
.profile-form input[type="checkbox"] {
  display: inline;
}
.profile-panel pre {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
.profile-panel button[aria-pressed="true"] {
  border-color: #2b688d;
  background: #e1eff7;
}
@media (max-width: 1100px) {
  .profile-columns {
    grid-template-columns: 1fr;
  }
}
@media (prefers-reduced-motion: reduce) {
  .profile-panel * {
    animation: none !important;
    transition: none !important;
  }
}
</style>
