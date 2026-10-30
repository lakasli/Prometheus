<script setup lang="ts">
import { onMounted, ref, reactive } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>(),
  items = ref<any[]>([]),
  policy = ref<any>(null),
  busy = ref(false),
  notice = ref("");
const form = reactive({ person: "", delta: 1, reason: "", from: "", into: "" });
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
  policy.value = rules;
}
async function save() {
  await act(async () => {
    policy.value = await api(
      "/relationships/policy",
      "PUT",
      policy.value.value,
      policy.value.revision,
    );
    notice.value = "好感规则已保存。";
    await load();
  });
}
async function event() {
  if (!confirm("确认此依据与分值？这不会授予主动联系权限。")) return;
  await act(async () => {
    await api(`/relationships/people/${form.person}/event`, "POST", {
      delta: form.delta,
      reason: form.reason,
      reviewed: true,
    });
    form.reason = "";
    await load();
    notice.value = "已保存人工确认的相处事件。";
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
    });
    await load();
    notice.value = "人物已合并，原编号作为别名保留。";
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
          分值依据已审核事件计算，按日向零衰减。分值不会授予主动联系或自动发送权限。
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
        <p>当前好感：{{ person.affinity }}</p>
        <p v-for="event in person.events" :key="event.id" class="muted">
          {{ event.delta > 0 ? "+" : "" }}{{ event.delta }} · {{ event.reason }}
        </p>
      </article>
    </div>
    <form v-if="items.length" @submit.prevent="event">
      <div class="form-row">
        <label
          >相处人物<select v-model="form.person" required>
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
      ><button class="primary" :disabled="busy">确认相处事件</button>
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
        <button class="secondary" :disabled="busy">保存好感规则</button>
      </form>
    </details>
    <details v-if="items.length > 1">
      <summary>核对并合并重复人物</summary>
      <form @submit.prevent="merge">
        <label
          >待合并人物<select v-model="form.from" required>
            <option value="" disabled>选择人物</option>
            <option v-for="person in items" :key="person.id" :value="person.id">
              {{ person.data.name }}
            </option>
          </select></label
        ><label
          >保留人物<select v-model="form.into" required>
            <option value="" disabled>选择人物</option>
            <option v-for="person in items" :key="person.id" :value="person.id">
              {{ person.data.name }}
            </option>
          </select></label
        ><button class="danger" :disabled="busy || form.from === form.into">
          确认合并人物
        </button>
      </form>
    </details>
  </section>
</template>
