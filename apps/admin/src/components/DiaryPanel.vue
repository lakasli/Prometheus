<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown]; dirty: [boolean]; changed: [] }>();
const policies = ref<any[]>([]),
  sources = ref<any[]>([]),
  entries = ref<any[]>([]),
  events = ref<any[]>([]),
  policyId = ref(""),
  busy = ref(false),
  notice = ref(""),
  selected = ref<any>(null),
  entryEdit = ref(false),
  eventId = ref(""),
  eventRevision = ref(0),
  entryTotal = ref(0),
  eventTotal = ref(0),
  entryOffset = ref(0),
  eventOffset = ref(0),
  status = ref<any>(null),
  query = ref("");
const periods: Record<string, string> = {
  day: "日记",
  week: "周记",
  month: "月记",
};
const labels: Record<string, string> = {
  sealed: "已封存",
  open: "日期尚在进行，仅记录截至当前",
  waiting_parents: "等待完整父日记",
  invalidated: "来源已失效",
  forgotten: "已遗忘",
  queued: "等待模型准入",
  running: "模型请求中",
  completed: "全部归并完成",
  idle: "尚未请求模型",
  empty: "无互动，不调用模型",
  failed: "失败，保留检查点",
  blocked: "条件未满足",
  interrupted: "已中断",
};
const reasons: Record<string, string> = {
  parents_missing: "部分日期尚未封存",
  parents_incomplete: "父内容覆盖尚不完整",
  daily_budget_wait: "等待滚动24小时模型额度",
  model_not_configured: "尚未配置模型",
  diary_input_limit: "输入超过预算",
  diary_output_truncated: "模型回复达到长度上限，未发布",
  diary_output_incomplete: "模型未正常结束",
  planning_invalid_json: "模型JSON无效或包含重复字段",
  restart_checkpoint: "重启后保留已有片段",
  daily_event_limit: "单日来源超过200条",
  source_grant_changed: "来源许可已改变",
  policy_changed: "分区规则已改变",
  rest_cycle_ended: "本次休息整理已结束",
};
const local = (at: number, timeZone = "Asia/Shanghai") => {
  const parts = new Intl.DateTimeFormat("sv-SE", {
    timeZone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
  }).format(new Date(at * 1000));
  return parts.replace(" ", "T");
};
const eventForm = reactive({
    content: "",
    happened_local: local(Date.now() / 1000),
    time_zone: "Asia/Shanghai",
    statement_type: "user_statement",
    visibility: "private",
    publish_confirmed: false,
  }),
  policyForm = reactive({
    id: "",
    revision: 0,
    source_key: "",
    visibility: "private",
    time_zone: "Asia/Shanghai",
    enabled: true,
    allow_model: false,
  }),
  consolidation = reactive({
    date: local(Date.now() / 1000).slice(0, 10),
    period: "day",
    model: false,
  }),
  manual = reactive({ facts: "", feelings: "" });
const eventBaseline = ref(JSON.stringify(eventForm)),
  policyBaseline = ref(JSON.stringify(policyForm)),
  manualBaseline = ref(JSON.stringify(manual));
const dirty = computed(
  () =>
    JSON.stringify(eventForm) !== eventBaseline.value ||
    JSON.stringify(policyForm) !== policyBaseline.value ||
    (entryEdit.value && JSON.stringify(manual) !== manualBaseline.value),
);
watch(dirty, (value) => emit("dirty", value));
const name = (p: any) =>
  p.source_key.startsWith("self:")
    ? `自己的明确记录 · ${p.visibility === "shared" ? "已确认共享" : "私有"}`
    : sources.value.find((s) => s.source_key === p.source_key)?.title ||
      "已授权会话来源";
const current = computed(() =>
  policies.value.find((p) => p.id === policyId.value),
);
const description = (value: string) => labels[value] ?? "待核对";
async function act(action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  try {
    await action();
  } catch (error) {
    emit("error", error);
  } finally {
    busy.value = false;
  }
}
function choose(p: any) {
  Object.assign(policyForm, {
    id: p.id,
    revision: p.revision,
    source_key: p.source_key,
    visibility: p.visibility,
    time_zone: p.time_zone,
    enabled: !!p.enabled,
    allow_model: !!p.allow_model,
  });
  policyBaseline.value = JSON.stringify(policyForm);
}
async function load() {
  const [p, s, st] = await Promise.all([
    api<any>("/diary/policies"),
    api<any>("/knowledge/sources"),
    api<any>("/diary/status"),
  ]);
  policies.value = p.items;
  sources.value = s.items;
  status.value = st;
  if (!policyId.value && p.items.length) {
    policyId.value = p.items[0].id;
    choose(p.items[0]);
  }
  const args = new URLSearchParams({
    limit: "10",
    ...(policyId.value ? { policy_id: policyId.value } : {}),
    ...(query.value ? { q: query.value } : {}),
  });
  const [e, v] = await Promise.all([
    api<any>(`/diary/entries?${args}&offset=${entryOffset.value}`),
    api<any>(`/diary/events?${args}&offset=${eventOffset.value}`),
  ]);
  entries.value = e.items;
  entryTotal.value = e.total;
  events.value = v.items;
  eventTotal.value = v.total;
}
async function selectPolicy() {
  if (
    JSON.stringify(policyForm) !== policyBaseline.value &&
    !confirm("放弃尚未保存的分区修改？")
  ) {
    policyId.value = policyForm.id;
    return;
  }
  const p = current.value;
  if (p) choose(p);
  entryOffset.value = eventOffset.value = 0;
  selected.value = null;
  await act(load);
}
function newPolicy() {
  if (
    JSON.stringify(policyForm) !== policyBaseline.value &&
    !confirm("放弃尚未保存的分区修改？")
  )
    return;
  Object.assign(policyForm, {
    id: "",
    revision: 0,
    source_key: "",
    visibility: "private",
    time_zone: "Asia/Shanghai",
    enabled: true,
    allow_model: false,
  });
  policyBaseline.value = JSON.stringify(policyForm);
}
async function savePolicy() {
  await act(async () => {
    const { id, revision, ...body } = policyForm;
    const p = await api<any>(
      id ? `/diary/policies/${id}` : "/diary/policies",
      id ? "PATCH" : "POST",
      { ...body, confirmed: true },
      id ? revision : undefined,
    );
    policyId.value = p.id;
    choose(p);
    await load();
    notice.value = "分区范围已保存，模型出域仍逐项遵守来源许可。";
  });
}
function resetEvent() {
  eventId.value = "";
  eventRevision.value = 0;
  Object.assign(eventForm, {
    content: "",
    happened_local: local(Date.now() / 1000),
    time_zone: "Asia/Shanghai",
    statement_type: "user_statement",
    visibility: "private",
    publish_confirmed: false,
  });
  eventBaseline.value = JSON.stringify(eventForm);
}
function clearEvent() {
  if (
    JSON.stringify(eventForm) === eventBaseline.value ||
    confirm("放弃尚未保存的事件？")
  )
    resetEvent();
}
function editEvent(e: any) {
  if (
    JSON.stringify(eventForm) !== eventBaseline.value &&
    !confirm("放弃尚未保存的事件编辑？")
  )
    return;
  eventId.value = e.id;
  eventRevision.value = e.revision;
  Object.assign(eventForm, {
    content: e.content,
    happened_local: local(e.happened_at, e.time_zone),
    time_zone: e.time_zone,
    statement_type: e.statement_type,
    visibility: e.visibility,
    publish_confirmed: e.visibility === "shared",
  });
  eventBaseline.value = JSON.stringify(eventForm);
}
let requestKey = crypto.randomUUID();
async function saveEvent() {
  await act(async () => {
    const body = eventId.value
      ? {
          content: eventForm.content,
          happened_local: eventForm.happened_local,
          time_zone: eventForm.time_zone,
          statement_type: eventForm.statement_type,
        }
      : { ...eventForm };
    await api(
      eventId.value ? `/diary/events/${eventId.value}` : "/diary/events",
      eventId.value ? "PATCH" : "POST",
      body,
      eventId.value ? eventRevision.value : undefined,
      undefined,
      eventId.value ? undefined : requestKey,
    );
    resetEvent();
    requestKey = crypto.randomUUID();
    await load();
    emit("changed");
    notice.value =
      "事件已保存。陈述类别和来源保留，模型尚未整理不代表没有记录。";
  });
}
async function forgetEvent(e: any) {
  if (
    !confirm(
      e.origin_kind === "record"
        ? "遗忘原资料及其事件、全部派生正文？原聊天与离线备份另行保留。"
        : "遗忘这条事件？其旧版本和全部日周月派生正文将清理，原聊天和离线备份另行保留。",
    )
  )
    return;
  await act(async () => {
    await api(`/diary/events/${e.id}`, "DELETE", undefined, e.revision);
    if (eventId.value === e.id) resetEvent();
    selected.value = null;
    await load();
    emit("changed");
    notice.value = "事件已遗忘，派生覆盖已失效。";
  });
}
async function open(e: any) {
  if (
    entryEdit.value &&
    JSON.stringify(manual) !== manualBaseline.value &&
    !confirm("放弃尚未保存的周期修订？")
  )
    return;
  await act(async () => {
    selected.value = await api<any>(`/diary/entries/${e.id}`);
    entryEdit.value = false;
  });
}
async function consolidate(retry = false) {
  if (!policyId.value) return;
  await act(async () => {
    selected.value = await api<any>("/diary/consolidate", "POST", {
      policy_id: policyId.value,
      ...consolidation,
      ...(retry ? { model: true, retry_failed: true } : {}),
    });
    entryEdit.value = false;
    await load();
    emit("changed");
    notice.value = "周期记录已准备。摘录、父目录、模型请求及全量归并分别记账。";
  });
}
async function refreshEntry() {
  if (!selected.value) return;
  await act(async () => {
    const fresh = await api<any>(`/diary/entries/${selected.value.id}`);
    if (!entryEdit.value) selected.value = fresh;
    else
      notice.value =
        "执行状态已读取；编辑内容仍保留，请重新载入后核对最新版本。";
    await load();
  });
}
async function rebaseManual() {
  if (
    !confirm(
      "核对当前有效来源并重建此人工周期？原人工正文将撤下，原始事件保留，重建后的摘录仍需单独申请模型整理。",
    )
  )
    return;
  await act(async () => {
    selected.value = await api<any>("/diary/consolidate", "POST", {
      policy_id: selected.value.policy_id,
      period: selected.value.period,
      date: selected.value.period_start,
      model: false,
      replace_manual: true,
      expected_revision: selected.value.revision,
    });
    entryEdit.value = false;
    await load();
    emit("changed");
    notice.value = "已按当前来源重建摘录，尚未请求模型。";
  });
}
function beginEdit() {
  Object.assign(manual, {
    facts: selected.value.version?.facts ?? "",
    feelings: selected.value.version?.feelings ?? "",
  });
  manualBaseline.value = JSON.stringify(manual);
  entryEdit.value = true;
}
async function saveEntry() {
  if (!confirm("将此周期保存为人工修订并锁定？自动整理将保留这份人工正文。"))
    return;
  await act(async () => {
    selected.value = await api<any>(
      `/diary/entries/${selected.value.id}`,
      "PATCH",
      manual,
      selected.value.revision,
    );
    entryEdit.value = false;
    await load();
    emit("changed");
    notice.value = "人工修订已保存并锁定。";
  });
}
async function forgetEntry() {
  if (
    !confirm(
      "遗忘整个周期？该周期不会自动重建，原始事件另行保留，相关派生正文将失效。",
    )
  )
    return;
  await act(async () => {
    selected.value = await api<any>(
      `/diary/entries/${selected.value.id}`,
      "DELETE",
      undefined,
      selected.value.revision,
    );
    entryEdit.value = false;
    await load();
    emit("changed");
  });
}
async function download() {
  await act(async () => {
    const q = new URLSearchParams({
        limit: "100",
        ...(policyId.value ? { policy_id: policyId.value } : {}),
      }),
      data = await api<any>(`/diary/export?${q}`),
      url = URL.createObjectURL(
        new Blob([JSON.stringify(data, null, 2)], { type: "application/json" }),
      ),
      a = document.createElement("a");
    a.href = url;
    a.download = "prometheus-diary-page.json";
    a.click();
    URL.revokeObjectURL(url);
    notice.value =
      "已导出当前分区最多100条事件和100份周期记录，文件包含总数与分页信息。";
  });
}
onMounted(() => act(load));
</script>
<template>
  <div class="feature-stack diary-ledger">
    <section class="panel">
      <div class="section-heading">
        <div>
          <h2>日记周期与来源覆盖</h2>
          <p>
            日记、周记与月记保留固定来源版本。感受单独记录，未观测的日期会明确标注。
          </p>
        </div>
        <button class="secondary" :disabled="busy" @click="act(load)">
          刷新周期与事件
        </button>
      </div>
      <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
      <p v-if="status && !status.clock_trusted" class="alert">
        主机时间尚未核验，自动封存和模型请求已阻止。
      </p>
      <label
        >当前日记分区<select v-model="policyId" @change="selectPolicy">
          <option v-for="p in policies" :key="p.id" :value="p.id">
            {{ name(p) }} · {{ p.time_zone }}
          </option>
        </select></label
      >
      <div class="form-row">
        <label
          >检索事件与周期正文<input v-model="query" maxlength="200" /></label
        ><button
          class="secondary"
          :disabled="busy"
          @click="
            entryOffset = eventOffset = 0;
            act(load);
          "
        >
          检索日记账本</button
        ><button class="secondary" :disabled="busy" @click="download">
          导出当前分区一页
        </button>
      </div>
      <details>
        <summary>日期整理与模型许可</summary>
        <p>
          默认保存自己的私有日期分区。会话采集和模型出域须在来源许可中分别授权；过去的未授权聊天不会回填。
        </p>
        <button class="secondary" :disabled="busy" @click="newPolicy">
          新建来源分区
        </button>
        <label
          >日记来源<select
            v-model="policyForm.source_key"
            :disabled="!!policyForm.id"
          >
            <option value="">请选择来源</option>
            <option
              v-for="p in policies.filter((p) =>
                p.source_key.startsWith('self:'),
              )"
              :key="p.id"
              :value="p.source_key"
            >
              自己的明确记录
            </option>
            <option
              v-for="s in sources"
              :key="s.source_key"
              :value="s.source_key"
            >
              {{ s.title }}
            </option>
          </select></label
        >
        <div class="form-row">
          <label
            >分区可见范围<select
              v-model="policyForm.visibility"
              :disabled="!!policyForm.id"
            >
              <option value="private">私有</option>
              <option value="shared">仅明确共享事件</option>
            </select></label
          ><label
            >日期所属IANA时区<input
              v-model="policyForm.time_zone"
              :readonly="
                !!policies.find((p) => p.id === policyForm.id)?.time_zone_locked
              "
              maxlength="128"
          /></label>
        </div>
        <label class="checkbox"
          ><input
            v-model="policyForm.enabled"
            type="checkbox"
          />启用此分区的日期整理</label
        ><label class="checkbox"
          ><input
            v-model="policyForm.allow_model"
            type="checkbox"
          />允许本分区的资料发送给模型整理</label
        >
        <p class="muted">
          更改已有规则会使旧派生正文失效。已有分区改时区需要明确迁移周期，不能静默重划日期。
        </p>
        <button
          class="primary"
          :disabled="busy || !policyForm.source_key"
          @click="savePolicy"
        >
          保存日期分区许可
        </button>
      </details>
      <div class="form-row">
        <label>整理日期<input v-model="consolidation.date" type="date" /></label
        ><label
          >整理周期<select v-model="consolidation.period">
            <option value="day">日记</option>
            <option value="week">周记</option>
            <option value="month">月记</option>
          </select></label
        >
      </div>
      <label class="checkbox"
        ><input
          v-model="consolidation.model"
          type="checkbox"
        />同时请求已获授权的模型整理</label
      ><button
        class="primary"
        :disabled="busy || !policyId"
        @click="consolidate(false)"
      >
        准备这个日记周期
      </button>
      <p class="muted">
        周一至周日为一周，月记只引用本月日记。单日最多200条来源，周期来源链不会截到前200条；有界模型片段全部归并后才发布完整正文。
      </p>
      <article v-for="e in entries" :key="e.id" class="job-card">
        <strong>{{ periods[e.period] }} · {{ e.period_start }}</strong>
        <p>
          {{ description(e.state) }} ·
          {{
            e.version?.generator?.startsWith("model:")
              ? "模型归并"
              : e.version?.generator === "manual"
                ? "人工修订"
                : e.period === "day"
                  ? "来源摘录"
                  : "父日记目录"
          }}
          · {{ e.coverage.sources }}条来源
        </p>
        <p>
          {{ e.coverage.complete_raw ? "正文覆盖已完成" : "正文覆盖尚未完成" }}
          · {{ description(e.task?.state) }}
        </p>
        <button class="secondary" :disabled="busy" @click="open(e)">
          查看周期与固定来源
        </button>
      </article>
      <div class="pagination">
        <span>共{{ entryTotal }}份周期记录</span>
        <div>
          <button
            class="secondary"
            :disabled="busy || entryOffset === 0"
            @click="
              entryOffset -= 10;
              act(load);
            "
          >
            上一页周期</button
          ><button
            class="secondary"
            :disabled="busy || entryOffset + 10 >= entryTotal"
            @click="
              entryOffset += 10;
              act(load);
            "
          >
            下一页周期
          </button>
        </div>
      </div>
    </section>
    <section v-if="selected" class="panel diary-detail">
      <div class="section-heading">
        <h2>周期正文与覆盖</h2>
        <button class="secondary" :disabled="busy" @click="refreshEntry">
          刷新所选周期状态
        </button>
      </div>
      <p>
        {{ periods[selected.period] }} · {{ selected.period_start }} 至
        {{ selected.period_end }}（不含终点） · {{ selected.time_zone }}
      </p>
      <p>
        {{ description(selected.state) }} ·
        {{ selected.coverage.sources }}条真实来源 ·
        {{ selected.coverage.parents.length }}份固定父日记
      </p>
      <p v-if="selected.version?.observation === 'unobserved'" class="muted">
        运行观测不完整；没有来源记录不代表当天没有经历。
      </p>
      <template v-if="!entryEdit"
        ><h3>事实与来源摘录</h3>
        <pre class="record-body">{{
          selected.version?.facts ?? "正文已失效或遗忘"
        }}</pre>
        <h3>明确的主观感受</h3>
        <p class="record-body">
          {{ selected.version?.feelings || "未记录感受" }}
        </p></template
      >
      <template v-else
        ><label
          >人工修订事实<textarea
            v-model="manual.facts"
            maxlength="12000"
            rows="6"
          /></label
        ><label
          >人工修订感受<textarea
            v-model="manual.feelings"
            maxlength="4000"
            rows="3"
          /></label
        ><button class="primary" :disabled="busy" @click="saveEntry">
          保存并锁定人工正文
        </button></template
      >
      <p v-if="selected.task">
        {{ description(selected.task.state) }} ·
        {{
          reasons[selected.task.error_code] ||
          (selected.task.error_code ? "条件已变化，请核对来源和许可" : "")
        }}<span v-if="selected.task.failures">
          · 连续失败{{ selected.task.failures }}次</span
        >
      </p>
      <div class="action-row">
        <button
          v-if="selected.locked && !selected.deleted_at"
          class="secondary"
          :disabled="busy"
          @click="rebaseManual"
        >
          核对来源并重建人工周期
        </button>
        <button
          v-if="
            !entryEdit &&
            !selected.deleted_at &&
            selected.state !== 'invalidated'
          "
          class="secondary"
          :disabled="busy"
          @click="beginEdit"
        >
          人工修订此周期</button
        ><button
          v-if="selected.task?.state === 'failed'"
          class="secondary"
          :disabled="busy"
          @click="
            consolidation.date = selected.period_start;
            consolidation.period = selected.period;
            consolidate(true);
          "
        >
          明确重试失败批次</button
        ><button
          v-if="!selected.deleted_at"
          class="secondary danger"
          :disabled="busy"
          @click="forgetEntry"
        >
          遗忘整个周期
        </button>
      </div>
      <details>
        <summary>版本、来源与模型请求证据</summary>
        <article
          v-for="v in selected.versions"
          :key="v.revision"
          class="job-card"
        >
          <strong
            >版本{{ v.revision }} ·
            {{
              v.generator.startsWith("model:")
                ? "模型归并"
                : v.generator === "manual"
                  ? "人工修订"
                  : "宿主证据记录"
            }}</strong
          >
          <p>
            {{ v.source_count }}条来源 ·
            {{ v.complete_raw ? "覆盖完整" : "覆盖未完成" }}
          </p>
          <pre class="record-body">{{ v.facts ?? "旧正文已清理" }}</pre>
        </article>
        <p v-for="s in selected.sources" :key="s.event_id" class="muted">
          {{
            s.origin_kind === "message"
              ? "完成互动"
              : s.origin_kind === "manual"
                ? "明确记录"
                : "原资料"
          }}
          · {{ s.event_id }} · 版本{{ s.event_revision
          }}{{ s.deleted_at ? " · 已遗忘" : "" }}
        </p>
        <p v-for="a in selected.attempts" :key="a.id" class="muted">
          {{ description(a.status) }} · 输入{{ a.input_bytes }}字节 ·
          {{
            a.output_metadata?.finish_reason === "length"
              ? "长度上限，未接受"
              : a.error_code
                ? reasons[a.error_code] || "请求失败"
                : "结果以覆盖状态为准"
          }}
        </p>
      </details>
    </section>
    <section class="panel">
      <h2>明确记录一件事</h2>
      <p>
        这里只保存自己的明确记录，不自动采集其他来源。事实、对方陈述与感受按类别保留。
      </p>
      <div class="form-row">
        <label
          >发生的民用时间<input
            v-model="eventForm.happened_local"
            type="datetime-local"
            step="1" /></label
        ><label
          >输入时间所属IANA时区<input
            v-model="eventForm.time_zone"
            maxlength="128"
        /></label>
      </div>
      <label
        >陈述类别<select v-model="eventForm.statement_type">
          <option value="user_statement">本人明确陈述</option>
          <option value="observed_fact">已核实事实</option>
          <option value="subjective">本人的主观感受</option>
        </select></label
      ><label
        >事件正文<textarea
          v-model="eventForm.content"
          maxlength="12000"
          rows="4"
        /></label
      ><label
        >事件可见范围<select
          v-model="eventForm.visibility"
          :disabled="!!eventId"
        >
          <option value="private">私有</option>
          <option value="shared">共享</option>
        </select></label
      ><label
        v-if="eventForm.visibility === 'shared' && !eventId"
        class="checkbox"
        ><input
          v-model="eventForm.publish_confirmed"
          type="checkbox"
        />明确确认发布这条共享事件</label
      >
      <div class="action-row">
        <button
          class="primary"
          :disabled="busy || !eventForm.content.trim()"
          @click="saveEvent"
        >
          {{ eventId ? "保存事件更正" : "保存明确事件" }}</button
        ><button class="secondary" @click="clearEvent">清空事件编辑</button>
      </div>
      <h3>来源事件与删除水位</h3>
      <article v-for="e in events" :key="e.id" class="job-card">
        <p>
          {{ local(e.happened_at, e.time_zone).replace("T", " ") }} ·
          {{ e.visibility === "shared" ? "共享" : "私有" }} ·
          {{
            e.origin_kind === "manual"
              ? "明确记录"
              : e.origin_kind === "message"
                ? "授权完成互动"
                : "原资料"
          }}
        </p>
        <p class="record-body">
          {{ e.content ?? "正文已遗忘或撤权，来源ID保留以阻止复活" }}
        </p>
        <div v-if="!e.deleted_at" class="action-row">
          <button
            v-if="e.origin_kind !== 'record'"
            class="secondary"
            :disabled="busy"
            @click="editEvent(e)"
          >
            更正这条事件</button
          ><button
            class="secondary danger"
            :disabled="busy"
            @click="forgetEvent(e)"
          >
            遗忘这条事件
          </button>
        </div>
        <p v-else-if="!e.deleted_at" class="muted">
          更正或撤销请使用原资料与来源许可入口。
        </p>
      </article>
      <div class="pagination">
        <span>共{{ eventTotal }}条事件（含删除水位）</span>
        <div>
          <button
            class="secondary"
            :disabled="busy || eventOffset === 0"
            @click="
              eventOffset -= 10;
              act(load);
            "
          >
            上一页事件</button
          ><button
            class="secondary"
            :disabled="busy || eventOffset + 10 >= eventTotal"
            @click="
              eventOffset += 10;
              act(load);
            "
          >
            下一页事件
          </button>
        </div>
      </div>
    </section>
  </div>
</template>
