<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { api } from "../api";
const props = defineProps<{ person: any; sources: any[]; targets: any[] }>(),
  emit = defineEmits<{ error: [unknown]; dirty: [boolean]; changed: [] }>();
const source = ref(""),
  targets = ref<string[]>([]),
  confirmed = ref(false),
  jobs = ref<any[]>([]),
  busy = ref(false),
  notice = ref(""),
  offset = ref(0),
  total = ref(0);
const options = computed(() =>
    props.sources.filter(
      (s) =>
        ["diary_event", "diary_entry"].includes(s.type) &&
        (s.visibility === "shared" || s.owner_id === props.person.owner_id) &&
        !(props.person.visibility === "shared" && s.visibility !== "shared"),
    ),
  ),
  people = computed(() =>
    props.targets.filter(
      (p) =>
        p.id !== props.person.id &&
        !p.merged_into &&
        p.owner_id === props.person.owner_id &&
        p.visibility === props.person.visibility,
    ),
  );
const dirty = computed(
  () => !!source.value || targets.value.length > 0 || confirmed.value,
);
watch(
  () => people.value.map((p) => p.id).join(","),
  () => {
    const kept = targets.value.filter((id) =>
      people.value.some((p) => p.id === id),
    );
    if (kept.length !== targets.value.length) {
      targets.value = kept;
      confirmed.value = false;
    }
  },
);
watch(dirty, (v) => emit("dirty", v));
let key = crypto.randomUUID(),
  disposed = false,
  version = 0,
  timer: ReturnType<typeof setTimeout> | undefined,
  polling = false;
const seen = new Map<string, string>();
watch(
  [source, targets, () => props.person.id],
  () => {
    key = crypto.randomUUID();
  },
  { deep: true },
);
watch(
  () => props.person.id,
  () => {
    source.value = "";
    targets.value = [];
    confirmed.value = false;
    offset.value = 0;
    jobs.value = [];
    total.value = 0;
    version++;
    seen.clear();
    void load();
  },
);
const names: Record<string, string> = {
  queued: "排队等待",
  running: "正在整理",
  cancelling: "正在结束请求",
  cancelled: "已取消",
  interrupted: "服务退出时中断，未自动重试",
  failed: "整理失败",
  invalidated: "来源或人物版本已失效",
  completed: "已生成待审核候选",
};
const reasons: Record<string, string> = {
  daily_budget_wait: "等待24小时预算恢复",
  model_not_configured: "尚未配置模型",
  profile_model_input_limit: "输入超过模型窗口或字节预算",
  context_provider_mismatch: "模型窗口声明与当前模型不一致",
  profile_model_self_approval: "模型错误地声称已经核实",
  profile_model_unlisted_target: "模型选择了未核对的关系目标",
  profile_model_output_limit: "模型输出超限或未完整结束",
  profile_model_subjective_claim:
    "主观日记不能升级为本人陈述或他人事实，请核对来源",
  operator_cancelled: "已明确取消",
  service_restarted: "服务重启，保留诊断且未重放请求",
  server_shutdown: "服务退出，未自动重试",
  automatic_work_not_allowed:
    "作息或运行条件变化，原请求已中断，请重新明确选择后排队",
  profile_model_timeout: "整理超过45秒，未自动重试",
};
async function load() {
  if (disposed || polling) return;
  polling = true;
  const token = version,
    pid = props.person.id;
  try {
    const data = await api<any>(
      `/relationships/model-jobs?person_id=${pid}&limit=20&offset=${offset.value}`,
    );
    if (disposed || token !== version) return;
    let changed = false;
    for (const j of data.items) {
      if (
        seen.has(j.id) &&
        seen.get(j.id) !== "completed" &&
        j.status === "completed"
      )
        changed = true;
      seen.set(j.id, j.status);
    }
    jobs.value = data.items;
    total.value = data.total;
    if (changed) emit("changed");
  } catch (e) {
    if (!disposed && token === version) emit("error", e);
  } finally {
    polling = false;
  }
}
async function submit() {
  if (busy.value) return;
  const s = options.value.find((s) => s.key === source.value);
  if (!s || !confirmed.value) return;
  const token = version;
  busy.value = true;
  try {
    const receipt = await api<any>(
      "/relationships/model-jobs",
      "POST",
      {
        person_id: props.person.id,
        source: { type: s.type, id: s.id, revision: s.revision },
        target_person_ids: targets.value,
        confirmed: true,
        idempotency_key: key,
      },
      props.person.revision,
    );
    if (disposed || token !== version) return;
    seen.set(receipt.id, "queued");
    notice.value = "已排队。完成后仍须逐项审核，尚未成为有效资料。";
    source.value = "";
    targets.value = [];
    confirmed.value = false;
    offset.value = 0;
    await load();
  } catch (e) {
    if (!disposed && token === version) emit("error", e);
  } finally {
    busy.value = false;
  }
}
async function cancel(j: any) {
  busy.value = true;
  try {
    await api(
      `/relationships/model-jobs/${j.id}/cancel`,
      "POST",
      {},
      j.revision,
    );
    await load();
  } catch (e) {
    emit("error", e);
  } finally {
    busy.value = false;
  }
}
async function page(delta: number) {
  offset.value += delta;
  jobs.value = [];
  version++;
  await load();
}
function poll() {
  timer = setTimeout(async () => {
    if (!document.hidden) await load();
    if (!disposed) poll();
  }, 1500);
}
onMounted(() => {
  void load();
  poll();
});
onUnmounted(() => {
  disposed = true;
  version++;
  if (timer) clearTimeout(timer);
  emit("dirty", false);
});
</script>
<template>
  <details class="profile-model-panel">
    <summary>从日记整理待审核人物资料</summary>
    <p>
      只发送本次选择的日记事实、当前人物称呼和勾选的同域目标。来源须已允许日记采集和模型整理；每次最多8个字段与4条有向关系，模型不能自行核实、合并身份或授予联系权限。
    </p>
    <form @submit.prevent="submit">
      <label
        >本次人物整理来源<select
          aria-label="本次人物整理来源"
          v-model="source"
          required
        >
          <option value="">选择一条已有日记事件或事实条目</option>
          <option v-for="s in options" :key="s.key" :value="s.key">
            {{ s.type === "diary_event" ? "事件" : "日记" }}：{{ s.label }}
          </option>
        </select></label
      >
      <fieldset>
        <legend>本次核对的关系目标（可不选，最多10人）</legend>
        <label v-for="p in people" :key="p.id"
          ><input
            type="checkbox"
            :value="p.id"
            v-model="targets"
            :disabled="targets.length >= 10 && !targets.includes(p.id)"
          />{{ p.data.name }}</label
        >
        <p v-if="!people.length">当前页没有同域目标，只整理当前人物字段。</p>
      </fieldset>
      <label
        ><input
          type="checkbox"
          v-model="confirmed"
          required
        />我已核对人物与所选来源，并同意本次发送给已配置模型</label
      >
      <div class="profile-job-tools">
        <button :disabled="busy || !source || !confirmed">
          排队生成待审核候选</button
        ><button type="button" :disabled="busy" @click="load">
          刷新整理进度</button
        ><button
          type="button"
          v-if="dirty"
          :disabled="busy"
          @click="
            source = '';
            targets = [];
            confirmed = false;
          "
        >
          清空本次选择
        </button>
      </div>
    </form>
    <p v-if="notice" role="status">{{ notice }}</p>
    <p>
      失败也计入来源域8次、全局24次的滚动24小时额度，并共享后台总预算。整理会在交互和睡眠安排允许时进行。
    </p>
    <article v-for="j in jobs" :key="j.id" class="profile-job">
      <strong>{{ names[j.status] ?? j.status }}</strong>
      <p>
        {{ j.source.type === "diary_event" ? "日记事件" : "日记事实条目" }} ·
        {{ new Date(j.created_at * 1000).toLocaleString("zh-CN") }}
      </p>
      <p v-if="j.error_code">
        {{
          reasons[j.error_code] ??
          "来源、人物或模型请求已变化，请核对后重新明确选择。"
        }}
      </p>
      <p v-if="j.status === 'completed'">
        {{ j.result.assertion_ids?.length ?? 0 }}个字段、{{
          j.result.edge_ids?.length ?? 0
        }}条关系候选，仍待逐项审核。
      </p>
      <p
        v-if="
          j.request_active && ['cancelling', 'invalidated'].includes(j.status)
        "
      >
        正在结束原请求，稍后刷新查看最终状态。
      </p>
      <button
        v-if="['queued', 'running'].includes(j.status)"
        :disabled="busy"
        @click="cancel(j)"
      >
        取消这次人物整理
      </button>
    </article>
    <div class="profile-job-tools">
      <button :disabled="busy || offset === 0" @click="page(-20)">
        上一批整理记录</button
      ><span>共{{ total }}条</span
      ><button :disabled="busy || offset + 20 >= total" @click="page(20)">
        下一批整理记录
      </button>
    </div>
  </details>
</template>
<style scoped>
.profile-model-panel {
  margin: 18px 0;
  padding: 12px;
  border: 1px solid var(--border, #d9dce2);
  border-radius: 8px;
}
.profile-model-panel label {
  display: block;
  margin: 12px 0;
}
.profile-model-panel select {
  display: block;
  width: 100%;
  max-width: 100%;
}
.profile-model-panel input[type="checkbox"] {
  display: inline-block;
  width: auto;
  margin-right: 8px;
  vertical-align: middle;
}
.profile-model-panel fieldset {
  min-width: 0;
}
.profile-job {
  padding: 12px;
  border-top: 1px solid var(--border, #d9dce2);
}
.profile-job-tools {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  align-items: center;
  margin: 12px 0;
}
.profile-model-panel p {
  overflow-wrap: anywhere;
}
</style>
