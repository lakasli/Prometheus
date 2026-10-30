<script setup lang="ts">
import { computed, onMounted, ref, reactive, watch } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>(),
  items = ref<any[]>([]),
  policy = ref<any>(null),
  busy = ref(false),
  notice = ref("");
const form = reactive({
  person: "",
  delta: 1,
  reason: "",
  from: "",
  into: "",
  mappingReason: "",
  split: "",
  splitReason: "",
  policyReason: "",
});
const roots = computed(() => items.value.filter((item) => !item.merged_into));
const merged = computed(() => items.value.filter((item) => item.merged_into));
const legacyMappings = ref<any[]>([]),
  history = ref<any[]>([]);
let eventKey = crypto.randomUUID();
watch(
  () => [form.person, form.delta, form.reason],
  () => {
    eventKey = crypto.randomUUID();
  },
);
async function act(action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  notice.value = "";
  try {
    await action();
  } catch (error) {
    emit("error", error);
  } finally {
    busy.value = false;
  }
}
async function load() {
  const [people, rules] = await Promise.all([
    api<any>("/relationships/people"),
    api<any>("/relationships/policy"),
  ]);
  items.value = people.items;
  legacyMappings.value = people.legacy_mappings;
  policy.value = rules;
  history.value = (await api<any>("/relationships/policy/history")).items;
}
async function save() {
  if (
    !confirm("确认修改好感规则？旧区间按原策略结算，初始值仅用于今后新建人物。")
  )
    return;
  await act(async () => {
    policy.value = await api(
      "/relationships/policy",
      "PUT",
      { ...policy.value.value, confirmed: true, reason: form.policyReason },
      policy.value.revision,
    );
    notice.value = "好感规则已保存。";
    await load();
  });
}
async function event() {
  if (
    !confirm(
      "确认此次人工调整的依据与分值？这不会重置直接互动宽限或授予联系权限。",
    )
  )
    return;
  await act(async () => {
    const person = items.value.find((item) => item.id === form.person);
    if (!person) return;
    await api(
      `/relationships/people/${form.person}/event`,
      "POST",
      {
        delta: form.delta,
        reason: form.reason,
        reviewed: true,
        event_key: eventKey,
        kind: "manual_adjustment",
      },
      person.revision,
    );
    form.reason = "";
    await load();
    notice.value = "已保存人工确认的好感调整。";
  });
}
async function merge() {
  const from = items.value.find((p) => p.id === form.from),
    into = items.value.find((p) => p.id === form.into);
  if (
    !from ||
    !into ||
    !confirm(`确认将“${from.data.name}”并入“${into.data.name}”？请先核对资料。`)
  )
    return;
  await act(async () => {
    await api(`/relationships/people/${from.id}/merge`, "POST", {
      into_id: into.id,
      expected_from_revision: from.revision,
      expected_into_revision: into.revision,
      confirmed: true,
      reason: form.mappingReason,
    });
    await load();
    notice.value = "人物映射已合并，原资料、关系和独立账本保留；可明确拆分。";
  });
}
async function split() {
  const person = items.value.find((item) => item.id === form.split),
    target = items.value.find((item) => item.id === person?.merged_into);
  if (
    !person ||
    !target ||
    !confirm(
      `确认解除“${person.data.name}”并入“${target.data.name}”的映射？原档案和账本继续独立。`,
    )
  )
    return;
  await act(async () => {
    await api(`/relationships/people/${person.id}/split`, "POST", {
      expected_from_revision: person.revision,
      expected_into_revision: target.revision,
      confirmed: true,
      reason: form.splitReason,
    });
    form.split = "";
    form.splitReason = "";
    await load();
    notice.value = "人物映射已拆分，原资料与账本保持独立。";
  });
}
onMounted(() => act(load));
</script>
<template>
  <section class="panel feature-section">
    <div class="section-heading">
      <div>
        <h2>相处记录与好感</h2>
        <p>
          分值使用独立账本，按已保存规则和宽限结算；规则修改不追溯重算旧区间。人工调整不等于真实互动，也不授予主动联系权限。
        </p>
      </div>
      <button class="secondary" :disabled="busy" @click="act(load)">
        刷新好感记录
      </button>
    </div>
    <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
    <div class="record-grid">
      <article v-for="person in items" :key="person.id" class="record-card">
        <h3>{{ person.data.name }}</h3>
        <p v-if="person.merged_into" class="muted">
          映射并入：{{
            items.find((item) => item.id === person.merged_into)?.data.name ||
            "另一个档案"
          }}；保留原档案和独立账本。
        </p>
        <p>当前好感：{{ person.affinity }}</p>
        <p v-for="event in person.events" :key="event.id" class="muted">
          {{ event.delta > 0 ? "+" : "" }}{{ event.delta }} · {{ event.reason }}
        </p>
      </article>
    </div>
    <form v-if="items.length" @submit.prevent="event">
      <div class="form-row">
        <label
          >相处人物<select v-model="form.person" aria-label="相处人物" required>
            <option value="" disabled>选择人物</option>
            <option v-for="person in items" :key="person.id" :value="person.id">
              {{ person.data.name }}
            </option>
          </select></label
        ><label
          >好感变化<input
            v-model.number="form.delta"
            type="number"
            :min="-(policy?.value.max_event_delta ?? 10)"
            :max="policy?.value.max_event_delta ?? 10"
            required
        /></label>
      </div>
      <label
        >相处依据<textarea
          v-model="form.reason"
          rows="2"
          maxlength="2000"
          required
        ></textarea></label
      ><button class="primary" :disabled="busy">确认人工调整</button>
    </form>
    <details v-if="policy">
      <summary>好感规则</summary>
      <form @submit.prevent="save">
        <div class="form-row">
          <label
            v-for="[key, label] in [
              ['initial', '初始好感'],
              ['min', '好感下限'],
              ['max', '好感上限'],
              ['decay_per_day', '每日衰减'],
              ['max_event_delta', '单次变化上限'],
              ['max_daily_delta', '每日绝对变化上限'],
              ['grace_seconds', '空闲宽限（秒）'],
              ['downtime_cap_seconds', '停机补算上限（秒）'],
            ]"
            :key="key"
            >{{ label
            }}<input
              v-model.number="policy.value[key]"
              type="number"
              :step="key === 'decay_per_day' ? '0.1' : '1'"
              required
          /></label>
        </div>
        <label
          >衰减方向<select v-model="policy.value.decay_mode">
            <option value="minimum">向规则下限衰减</option>
            <option value="zero">向零衰减（兼容既有规则）</option>
          </select></label
        >
        <label
          >规则修改依据<textarea
            v-model="form.policyReason"
            maxlength="2000"
            required
            rows="2"
          ></textarea>
        </label>
        <button class="secondary" :disabled="busy">保存好感规则</button>
      </form>
      <p class="muted">
        新规则量表0至100，单次最多5、每日绝对累计最多10；已有自定义规则不会因升级被覆盖。人工调整不重置宽限。直接互动计分仍待人物身份与真实来源验证。
      </p>
      <details>
        <summary>好感策略历史</summary>
        <p v-for="entry in history" :key="entry.revision">
          版本 {{ entry.revision }} · {{ entry.reason }}
        </p>
      </details>
    </details>
    <details v-if="items.length > 1">
      <summary>核对并合并重复人物</summary>
      <form @submit.prevent="merge">
        <label
          >待合并人物<select
            v-model="form.from"
            aria-label="待合并人物"
            required
          >
            <option value="" disabled>选择人物</option>
            <option v-for="person in roots" :key="person.id" :value="person.id">
              {{ person.data.name }}
            </option>
          </select></label
        ><label
          >保留人物<select v-model="form.into" aria-label="保留人物" required>
            <option value="" disabled>选择人物</option>
            <option v-for="person in roots" :key="person.id" :value="person.id">
              {{ person.data.name }}
            </option>
          </select></label
        ><label
          >合并依据<textarea
            v-model="form.mappingReason"
            maxlength="2000"
            rows="2"
            required
          ></textarea></label
        ><button class="danger" :disabled="busy || form.from === form.into">
          确认合并人物
        </button>
      </form>
    </details>
    <details v-if="merged.length">
      <summary>核对并拆分人物映射</summary>
      <form @submit.prevent="split">
        <label
          >待拆分人物<select
            v-model="form.split"
            aria-label="待拆分人物"
            required
          >
            <option value="" disabled>选择已合并来源</option>
            <option
              v-for="person in merged"
              :key="person.id"
              :value="person.id"
            >
              {{ person.data.name }}
            </option>
          </select></label
        ><label
          >拆分依据<textarea
            v-model="form.splitReason"
            maxlength="2000"
            rows="2"
            required
          ></textarea></label
        ><button class="secondary" :disabled="busy">确认拆分人物</button>
      </form>
    </details>
    <p v-for="item in legacyMappings" :key="item.alias_id" class="info-note">
      历史映射 {{ item.alias_id.slice(0, 8) }}：{{ item.reason }}
    </p>
  </section>
</template>
