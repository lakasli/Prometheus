<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { api } from "../api";
const props = defineProps<{ person: any; sources: any[] }>(),
  emit = defineEmits<{ error: [unknown]; dirty: [boolean]; changed: [] }>();
const ledger = ref<any>(null),
  messages = ref<any[]>([]),
  policy = ref<any>(null),
  policyHistory = ref<any[]>([]),
  busy = ref(false),
  notice = ref(""),
  editing = ref(false),
  preview = ref<any>(null),
  policyBase = ref(0),
  policyReason = ref(""),
  idleDays = ref(7),
  policyForm = ref<any>(null);
const form = reactive({
    kind: "manual_adjustment",
    delta: 1,
    reason: "",
    source: "manual",
  }),
  dirty = computed(
    () =>
      editing.value ||
      form.reason !== "" ||
      form.source !== "manual" ||
      form.kind !== "manual_adjustment" ||
      form.delta !== 1,
  );
watch(dirty, (v) => emit("dirty", v));
let key = crypto.randomUUID(),
  version = 0,
  disposed = false;
watch(
  () => [form.kind, form.delta, form.reason, form.source, props.person.id],
  () => {
    key = crypto.randomUUID();
  },
);
const options = computed(() =>
  form.kind === "direct_interaction"
    ? messages.value.map((s) => ({
        ...s,
        key: `message:${s.id}`,
        label: s.excerpt,
      }))
    : props.sources.map((s) => ({ ...s, key: `${s.type}:${s.id}` })),
);
watch(
  () => form.kind,
  (kind) => {
    form.source = kind === "manual_adjustment" ? "manual" : "";
  },
);
async function action(fn: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  try {
    await fn();
  } catch (e) {
    emit("error", e);
  } finally {
    busy.value = false;
  }
}
async function load() {
  const token = version,
    id = props.person.id;
  const [l, s, p, h] = await Promise.all([
    api(`/relationships/profiles/${id}/affinity`),
    api<any>(`/relationships/profiles/${id}/affinity/sources`),
    api(`/relationships/profiles/${id}/affinity/policy`),
    api<any>(`/relationships/profiles/${id}/affinity/policy-history`),
  ]);
  if (disposed || token !== version) return;
  ledger.value = l;
  messages.value = s.items;
  policy.value = p;
  policyHistory.value = h.items;
}
function reset() {
  Object.assign(form, {
    kind: "manual_adjustment",
    delta: 1,
    reason: "",
    source: "manual",
  });
  editing.value = false;
  policyReason.value = "";
  preview.value = null;
}
watch(
  () => props.person.id,
  () => {
    version++;
    ledger.value = null;
    messages.value = [];
    reset();
    void action(load);
  },
);
async function reload() {
  if (dirty.value && !confirm("刷新会放弃尚未保存的计分或规则修改，确定继续？"))
    return;
  reset();
  await action(load);
}
async function submit() {
  if (!ledger.value) return;
  const source =
    form.source === "manual"
      ? { type: "manual" }
      : options.value.find((s) => s.key === form.source);
  if (!source) return;
  if (
    !confirm(
      "确认本次依据、来源和分值？同一用户消息不能重复计分，好感不授予联系权限。",
    )
  )
    return;
  await action(async () => {
    const token = version,
      id = props.person.id;
    const result = await api(
      `/relationships/profiles/${id}/affinity/events`,
      "POST",
      {
        kind: form.kind,
        delta: form.delta,
        reason: form.reason,
        event_key: key,
        reviewed: true,
        source: { type: source.type, id: source.id, revision: source.revision },
      },
      ledger.value.revision,
    );
    if (disposed || token !== version) return;
    ledger.value = result;
    reset();
    await load();
    notice.value = "已保存独立账本事件，联系许可未改变。";
    emit("changed");
  });
}
async function revoke(e: any) {
  if (
    !confirm(
      "撤销这次计分并按原策略重放剩余事件？原因会清除，今日绝对额度不退还，原用户消息不能再次计分。",
    )
  )
    return;
  await action(async () => {
    ledger.value = await api(
      `/relationships/profiles/${props.person.id}/affinity/events/${e.id}/revoke`,
      "POST",
      {},
      ledger.value.revision,
    );
    await load();
    notice.value = "计分已撤销并重放，原记录的内容已清除。";
    emit("changed");
  });
}
function edit() {
  policyForm.value = structuredClone(policy.value.value);
  policyBase.value = policy.value.revision;
  policyReason.value = "";
  preview.value = null;
  editing.value = true;
}
async function project() {
  await action(async () => {
    preview.value = await api(
      `/relationships/profiles/${props.person.id}/affinity/policy-preview`,
      "POST",
      {
        policy: policyForm.value,
        idle_seconds: Math.round(idleDays.value * 86400),
      },
    );
  });
}
async function savePolicy() {
  if (
    !confirm(
      "保存未来规则？旧区间先按旧规则结算，初始值只用于之后新建的人物，不重置已有分值。",
    )
  )
    return;
  await action(async () => {
    await api(
      `/relationships/profiles/${props.person.id}/affinity/policy`,
      "PUT",
      { ...policyForm.value, confirmed: true, reason: policyReason.value },
      policyBase.value,
    );
    editing.value = false;
    await load();
    notice.value = "分段规则已保存，同一来源域使用该版本，人物账本继续独立。";
    emit("changed");
  });
}
const fmt = (v: any) =>
    Number(v).toLocaleString("zh-CN", { maximumFractionDigits: 6 }),
  date = (v: any) =>
    v === null || v === undefined
      ? "未记录"
      : new Date(Number(v) * 1000).toLocaleString("zh-CN");
onMounted(() => void action(load));
onUnmounted(() => {
  disposed = true;
  version++;
  emit("dirty", false);
});
</script>
<template>
  <details class="affinity-ledger">
    <summary>有来源的相处账本与重放</summary>
    <p>
      分值只记录本实例在这一来源域中的相处状态，不代表对方感受或人的价值，也不是接收许可。跨账号关联和同域合并不累加分数。
    </p>
    <p v-if="notice" role="status">{{ notice }}</p>
    <template v-if="ledger"
      ><p>
        <strong>当前分值：{{ fmt(ledger.affinity) }}</strong> · 最近直接互动：{{
          date(ledger.last_direct_at)
        }}
      </p>
      <p>结算至 {{ date(ledger.settled_at) }}。主动发送不可用。</p>
      <form @submit.prevent="submit">
        <label
          >本次账本事件<select aria-label="本次账本事件" v-model="form.kind">
            <option value="manual_adjustment">
              人工调整（不刷新直接互动时间）
            </option>
            <option value="direct_interaction">已核验私聊中的直接互动</option>
          </select></label
        ><label
          >计分证据<select aria-label="计分证据" v-model="form.source" required>
            <option v-if="form.kind === 'manual_adjustment'" value="manual">
              独立人工依据
            </option>
            <option value="">请选择明确证据</option>
            <option v-for="s in options" :key="s.key" :value="s.key">
              {{
                s.type === "message"
                  ? "用户消息"
                  : s.type === "memory"
                    ? "记忆"
                    : s.type === "diary_event"
                      ? "日记事件"
                      : "日记事实"
              }}：{{ s.label }}
            </option>
          </select></label
        >
        <p v-if="form.kind === 'direct_interaction' && !messages.length">
          当前没有可计分的完成用户消息；须先核对并绑定该人物的本人私聊，同一消息只使用一次。
        </p>
        <label
          >变化分值<input
            type="number"
            v-model.number="form.delta"
            :min="-(policy?.value.max_event_delta ?? 5)"
            :max="policy?.value.max_event_delta ?? 5"
            step="0.000001"
            required /></label
        ><label
          >本次计分依据<textarea
            v-model="form.reason"
            required
            maxlength="2000"
          />
        </label>
        <div class="affinity-actions">
          <button :disabled="busy || !form.source">核对并记录计分</button
          ><button type="button" :disabled="busy" @click="reload">
            刷新相处账本</button
          ><button type="button" v-if="dirty" :disabled="busy" @click="reset">
            放弃账本修改
          </button>
        </div>
      </form>
      <details>
        <summary>原策略与未来规则</summary>
        <p v-if="policy">
          当前版本{{ policy.revision }}：初始{{ policy.value.initial }}，范围{{
            policy.value.min
          }}–{{ policy.value.max }}，每日衰减{{
            policy.value.decay_per_day
          }}；宽限{{ policy.value.grace_seconds }}秒，停机补算上限{{
            policy.value.downtime_cap_seconds
          }}秒。每日变化绝对额度{{
            policy.value.max_daily_delta
          }}，按UTC日期统计 （上海时间每天08:00重置），撤销计分不退额。
        </p>
        <button :disabled="busy" @click="edit">调整未来相处规则</button>
        <form v-if="editing" @submit.prevent="savePolicy">
          <label
            v-for="[field, label] in [
              ['initial', '新人物初始值'],
              ['min', '分值下限'],
              ['max', '分值上限'],
              ['decay_per_day', '每日衰减'],
              ['grace_seconds', '直接互动宽限（秒）'],
              ['downtime_cap_seconds', '停机补算上限（秒）'],
              ['max_event_delta', '单次变化上限'],
              ['max_daily_delta', '每日变化绝对额度'],
            ]"
            :key="field"
            >{{ label
            }}<input
              v-model.number="policyForm[field!]"
              type="number"
              :step="field === 'decay_per_day' ? '0.000001' : '1'"
              required /></label
          ><label
            >规则修改依据<textarea
              v-model="policyReason"
              required
              maxlength="2000"
            /></label
          ><label
            >预演空闲天数<input
              v-model.number="idleDays"
              type="number"
              min="0"
              max="3650"
              step="0.001"
          /></label>
          <div class="affinity-actions">
            <button type="button" :disabled="busy" @click="project">
              只预演相处规则</button
            ><button :disabled="busy">确认保存未来规则</button
            ><button type="button" @click="editing = false">
              取消规则修改
            </button>
          </div>
          <p v-if="preview">
            仅从新初值预演：{{
              fmt(preview.projected_from_initial)
            }}，未改写已有账本或许可。
          </p>
        </form>
        <article v-for="h in policyHistory" :key="h.revision">
          <p>
            版本{{ h.revision }} · {{ date(h.effective_at) }} · {{ h.reason }}
          </p>
        </article>
      </details>
      <details open>
        <summary>事件与撤销记录</summary>
        <article v-for="e in ledger.events" :key="e.id" class="affinity-entry">
          <strong
            >{{ e.kind === "direct_interaction" ? "直接互动" : "人工调整" }} ·
            {{ e.invalidated_at ? "已失效" : fmt(e.delta) }}</strong
          >
          <p>
            {{ date(e.created_at) }} · 原请求{{
              fmt((e.requested_micro ?? e.delta * 1000000) / 1000000)
            }}
          </p>
          <p>{{ e.reason ?? "原因已清除，保留防重复及额度记录。" }}</p>
          <small>{{
            e.source_type === "message"
              ? "完成用户消息"
              : e.source_type === "diary_event"
                ? "日记事件"
                : e.source_type === "diary_entry"
                  ? "日记事实条目"
                  : e.source_type === "memory"
                    ? "记忆"
                    : e.source_type === "manual"
                      ? "人工依据"
                      : "迁移检查点之前的兼容记录"
          }}</small
          ><button
            v-if="e.source_type && !e.invalidated_at"
            :disabled="busy"
            @click="revoke(e)"
          >
            撤销这次计分
          </button>
        </article>
        <p v-if="!ledger.events.length">
          暂无事件。已有迁移前分值按检查点保留，不推测缺失来源。
        </p>
      </details>
      <details>
        <summary>来源失效后的重算记录</summary>
        <p v-for="r in ledger.replays" :key="r.id">
          {{ date(r.created_at) }}：{{
            fmt(r.previous_score_micro / 1000000)
          }}
          → {{ fmt(r.score_micro / 1000000) }}，{{
            r.active_events
          }}个有效事件，原策略和停机水位已保留。
        </p>
        <p v-if="!ledger.replays.length">尚无来源撤销重算。</p>
      </details></template
    >
  </details>
</template>
<style scoped>
.affinity-ledger {
  border: 1px solid var(--border, #d9dce2);
  border-radius: 8px;
  padding: 12px;
  margin: 18px 0;
  min-width: 0;
}
.affinity-ledger label {
  display: block;
  margin: 12px 0;
}
.affinity-ledger select,
.affinity-ledger input,
.affinity-ledger textarea {
  display: block;
  max-width: 100%;
  width: 100%;
}
.affinity-ledger p,
.affinity-ledger small {
  overflow-wrap: anywhere;
}
.affinity-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin: 12px 0;
}
.affinity-entry {
  padding: 12px 0;
  border-bottom: 1px solid var(--border, #d9dce2);
}
.affinity-entry button {
  display: block;
  margin-top: 8px;
}
</style>
