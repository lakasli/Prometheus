<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { api } from "../api";
const props = defineProps<{ refreshToken?: unknown }>();
const emit = defineEmits<{ error: [unknown]; dirty: [boolean]; changed: [] }>();
const busy = ref(false),
  notice = ref(""),
  items = ref<any[]>([]),
  total = ref(0),
  offset = ref(0),
  query = ref(""),
  selected = ref<any>(null),
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
const draft = computed(() => !!mode.value);
watch(draft, (v) => emit("dirty", v));
onUnmounted(() => emit("dirty", false));
const sourceOptions = computed(() =>
  sources.value.filter(
    (s) =>
      selected.value &&
      (s.visibility === "shared" || s.owner_id === selected.value.owner_id) &&
      !(selected.value.visibility === "shared" && s.visibility !== "shared"),
  ),
);
function begin(next: string) {
  if (mode.value && !confirm("放弃未保存的人物修改？")) return;
  mode.value = next;
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
  });
  notice.value = "";
  history.value = [];
}
async function act(action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  try {
    await action();
  } catch (e) {
    emit("error", e);
  } finally {
    busy.value = false;
  }
}
async function load() {
  const p = await api<any>(
    `/relationships/profiles?limit=50&offset=${offset.value}&q=${encodeURIComponent(query.value)}`,
  );
  items.value = p.items;
  total.value = p.total;
  graph.value = await api(
    `/relationships/graph?offset=${offset.value}&q=${encodeURIComponent(query.value)}`,
  );
}
async function select(id: string) {
  if (mode.value && !confirm("放弃未保存的人物修改？")) return;
  await act(async () => {
    selected.value = await api(`/relationships/profiles/${id}`);
    mode.value = "";
    history.value = [];
  });
}
async function refresh() {
  await load();
  if (selected.value)
    selected.value = await api(`/relationships/profiles/${selected.value.id}`);
}
watch(
  () => props.refreshToken,
  () => {
    if (!mode.value) void act(refresh);
  },
);
async function saved(p: any) {
  selected.value = p;
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
    selected.value = null;
    mode.value = "";
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
const coords = reactive<Record<string, { x: number; y: number }>>({}),
  view = reactive({ x: 0, y: 0, zoom: 1 });
watch(graph, (g) => {
  for (const [i, p] of (g?.items ?? []).entries())
    if (!coords[p.id])
      coords[p.id] = { x: 95 + (i % 5) * 155, y: 70 + Math.floor(i / 5) * 105 };
});
let drag:
  | { id: string; x: number; y: number; original: { x: number; y: number } }
  | undefined;
function down(e: PointerEvent, id: string) {
  if (e.button !== 0) return;
  e.preventDefault();
  (e.currentTarget as Element).setPointerCapture(e.pointerId);
  const point = graphPoint(e);
  drag = {
    id,
    x: point.x,
    y: point.y,
    original: { ...(id === "pan" ? view : coords[id]!) },
  };
}
function move(e: PointerEvent) {
  if (!drag) return;
  const point = graphPoint(e);
  const scale = drag.id === "pan" ? 1 : view.zoom,
    target = drag.id === "pan" ? view : coords[drag.id]!;
  target.x = drag.original.x + (point.x - drag.x) / scale;
  target.y = drag.original.y + (point.y - drag.y) / scale;
}
function graphPoint(e: PointerEvent) {
  const target = e.currentTarget as SVGElement,
    svg = target instanceof SVGSVGElement ? target : target.ownerSVGElement,
    matrix = svg?.getScreenCTM()?.inverse();
  return matrix
    ? new DOMPoint(e.clientX, e.clientY).matrixTransform(matrix)
    : { x: e.clientX, y: e.clientY };
}
function position(id: string) {
  return coords[id] ?? { x: 0, y: 0 };
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
    <form
      class="profile-tools"
      @submit.prevent="
        act(async () => {
          offset = 0;
          await load();
        })
      "
    >
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
          :aria-pressed="selected?.id === p.id"
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
        <button
          :disabled="offset === 0 || busy"
          @click="
            act(async () => {
              offset -= 50;
              await load();
            })
          "
        >
          上一页</button
        ><button
          :disabled="offset + 50 >= total || busy"
          @click="
            act(async () => {
              offset += 50;
              await load();
            })
          "
        >
          下一页
        </button>
      </nav>
      <div class="profile-detail">
        <template v-if="selected"
          ><h3>{{ selected.data.name }}</h3>
          <p>{{ selected.data.notes }}</p>
          <div class="profile-tools">
            <button @click="begin('rename')">更正名称与备注</button
            ><button @click="begin('field')">新增字段陈述</button
            ><button @click="begin('edge')">新增有向关系</button
            ><button @click="begin('bind')">核对账号身份</button
            ><button @click="begin('link')">关联跨账号身份</button
            ><button @click="begin('contact')">联系许可记录</button
            ><button class="danger" @click="forget">遗忘人物</button>
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
                v-if="a.status === 'pending'"
                @click="review(a, 'assertions', 'approve')"
              >
                审核批准</button
              ><button
                v-if="a.status === 'pending'"
                @click="review(a, 'assertions', 'reject')"
              >
                拒绝</button
              ><button @click="showHistory(a, 'assertions')">版本记录</button
              ><button @click="review(a, 'assertions', 'forget')">
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
                v-if="e.status === 'pending'"
                @click="review(e, 'edges', 'approve')"
              >
                审核批准</button
              ><button
                v-if="e.status === 'pending'"
                @click="review(e, 'edges', 'reject')"
              >
                拒绝</button
              ><button @click="showHistory(e, 'edges')">版本记录</button
              ><button @click="review(e, 'edges', 'forget')">遗忘关系</button>
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
            <button @click="unlink">拆分关联</button>
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
                          : "名称与备注"
            }}
          </h3>
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
    <details v-if="graph">
      <summary>本页关系图（最多50个人物，仅显示已批准的有向关系）</summary>
      <div class="profile-tools">
        <button @click="view.zoom = Math.min(2, view.zoom + 0.2)">放大</button
        ><button @click="view.zoom = Math.max(0.3, view.zoom - 0.2)">
          缩小</button
        ><button @click="view.x -= 80">左移</button
        ><button @click="view.x += 80">右移</button
        ><button @click="view.y -= 80">上移</button
        ><button @click="view.y += 80">下移</button
        ><button @click="Object.assign(view, { x: 0, y: 0, zoom: 1 })">
          复位
        </button>
      </div>
      <p>
        拖动空白平移，拖动节点调整位置；双击节点打开档案。列表提供同等操作。
      </p>
      <svg
        class="profile-graph"
        viewBox="0 0 800 500"
        role="img"
        aria-label="当前页已批准人物关系图"
        @pointerdown.self="down($event, 'pan')"
        @pointermove="move"
        @pointerup="drag = undefined"
        @pointercancel="drag = undefined"
      >
        <defs>
          <marker
            id="profile-arrow"
            viewBox="0 0 10 10"
            refX="9"
            refY="5"
            markerWidth="8"
            markerHeight="8"
            orient="auto-start-reverse"
          >
            <path d="M0,0 L10,5 L0,10Z" fill="currentColor" />
          </marker>
        </defs>
        <g :transform="`translate(${view.x} ${view.y}) scale(${view.zoom})`">
          <g v-for="e in graph.edges" :key="e.id">
            <line
              :x1="position(e.from_id).x"
              :y1="position(e.from_id).y"
              :x2="position(e.to_id).x"
              :y2="position(e.to_id).y"
              stroke="currentColor"
              marker-end="url(#profile-arrow)"
            />
            <text
              :x="(position(e.from_id).x + position(e.to_id).x) / 2"
              :y="(position(e.from_id).y + position(e.to_id).y) / 2 - 8"
            >
              {{ e.relationship_type }}
            </text>
          </g>
          <g
            v-for="p in graph.items"
            :key="p.id"
            :transform="`translate(${position(p.id).x} ${position(p.id).y})`"
            @pointerdown.stop="down($event, p.id)"
            @dblclick="select(p.id)"
          >
            <circle r="31" />
            <text text-anchor="middle" dy="5">
              {{ p.data.name.slice(0, 7) }}
            </text>
            <title>{{ p.data.name }}</title>
          </g>
        </g>
      </svg>
    </details>
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
  grid-template-columns: minmax(160px, 220px) minmax(0, 1fr);
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
.profile-graph {
  width: 100%;
  height: 500px;
  background: var(--surface, #f8f9fb);
  border: 1px solid #aab3bf;
  touch-action: none;
}
.profile-graph circle {
  fill: #f3efdf;
  stroke: #6e7d8d;
}
.profile-graph text {
  font-size: 12px;
  fill: #243543;
}
.profile-panel pre {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
.profile-panel button[aria-pressed="true"] {
  border-color: #2b688d;
  background: #e1eff7;
}
@media (max-width: 760px) {
  .profile-columns {
    grid-template-columns: 1fr;
  }
  .profile-graph {
    height: 340px;
  }
}
@media (prefers-reduced-motion: reduce) {
  .profile-panel * {
    animation: none !important;
    transition: none !important;
  }
}
</style>
