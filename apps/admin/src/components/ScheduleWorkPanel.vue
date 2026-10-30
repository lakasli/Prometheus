<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { api } from "../api";
import { scheduleReason, scheduleStatus } from "../schedule-labels";
const emit = defineEmits<{ error: [unknown]; dirty: [boolean]; changed: [] }>(),
  busy = ref(false),
  notice = ref(""),
  books = ref<any[]>([]),
  grants = ref<any[]>([]),
  plans = ref<any[]>([]),
  bookDetail = ref<any>(null),
  planPreview = ref<any>(null);
const today = new Intl.DateTimeFormat("en-CA", {
    timeZone: "Asia/Shanghai",
  }).format(new Date()),
  future = new Date(`${today}T00:00:00Z`);
future.setUTCDate(future.getUTCDate() + 7);
const book = reactive({ title: "", content: "", confirmed: false });
const grant = reactive<Record<string, any>>({
  id: "",
  revision: 0,
  title: "",
  resource_ids: [],
  allowed_types: ["reading"],
  time_zone: "Asia/Shanghai",
  from: `${today}T00:00:00`,
  until: `${future.toISOString().slice(0, 10)}T23:59:00`,
  horizon_days: 7,
  window_start: "09:00",
  window_end: "21:00",
  max_daily_activities: 4,
  max_daily_minutes: 120,
  max_daily_changes: 8,
  max_daily_model_calls: 8,
  max_daily_tokens: 24000,
  max_output_tokens: 1200,
  confirmed: false,
});
const plan = reactive({
  id: "",
  revision: 0,
  goal: "",
  grant_id: "",
  resource_ids: [] as string[],
  duration_minutes: 15,
  planning_mode: "bounded_rule",
  enabled: false,
});
const bookBase = ref(JSON.stringify(book)),
  grantBase = ref(JSON.stringify(grant)),
  planBase = ref(JSON.stringify(plan));
const dirty = computed(
  () =>
    busy.value ||
    JSON.stringify(book) !== bookBase.value ||
    JSON.stringify(grant) !== grantBase.value ||
    JSON.stringify(plan) !== planBase.value,
);
watch(dirty, (value) => emit("dirty", value));
onUnmounted(() => emit("dirty", false));
const activeBooks = computed(() =>
    books.value.filter((item) => !item.revoked_at),
  ),
  planBooks = computed(() =>
    activeBooks.value.filter((item) =>
      grants.value
        .find((g) => g.id === plan.grant_id)
        ?.policy.resource_ids.includes(item.id),
    ),
  );
watch(
  () => plan.grant_id,
  () => {
    plan.resource_ids = plan.resource_ids.filter((id) =>
      planBooks.value.some((item) => item.id === id),
    );
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
  const [a, b, c] = await Promise.all([
    api<any>("/schedule/resources"),
    api<any>("/schedule/autonomy-grants"),
    api<any>("/schedule/plans"),
  ]);
  books.value = a.items;
  grants.value = b.items;
  plans.value = c.items;
}
async function file(event: Event) {
  const input = event.target as HTMLInputElement,
    chosen = input.files?.[0];
  if (!chosen) return;
  try {
    if (chosen.size > 512 * 1024) throw new Error("文本文件不能超过512KiB");
    book.content = new TextDecoder("utf-8", { fatal: true }).decode(
      await chosen.arrayBuffer(),
    );
    if (!book.title) book.title = chosen.name.slice(0, 120);
  } catch (error) {
    emit("error", error);
  } finally {
    input.value = "";
  }
}
async function saveBook() {
  await act(async () => {
    await api("/schedule/resources", "POST", { ...book, type: "text" });
    book.title = "";
    book.content = "";
    book.confirmed = false;
    bookBase.value = JSON.stringify(book);
    await load();
    notice.value = "私人文本已授权保存，尚不代表已阅读。";
    emit("changed");
  });
}
async function showBook(item: any) {
  await act(async () => {
    bookDetail.value = await api(`/schedule/resources/${item.id}`);
  });
}
async function revokeBook(item: any) {
  if (
    !confirm(
      `撤销“${item.title}”的正文、覆盖水位与派生笔记？在途旧结果不能重新写回。`,
    )
  )
    return;
  await act(async () => {
    await api(
      `/schedule/resources/${item.id}/revoke`,
      "POST",
      {},
      item.revision,
    );
    if (bookDetail.value?.id === item.id)
      bookDetail.value = await api(`/schedule/resources/${item.id}`);
    await load();
    notice.value = "文本授权和派生笔记已撤销。";
    emit("changed");
  });
}
function grantPolicy() {
  const result: Record<string, any> = {};
  for (const key of [
    "title",
    "resource_ids",
    "allowed_types",
    "time_zone",
    "horizon_days",
    "window_start",
    "window_end",
    "max_daily_activities",
    "max_daily_minutes",
    "max_daily_changes",
    "max_daily_model_calls",
    "max_daily_tokens",
    "max_output_tokens",
  ])
    result[key] = grant[key];
  return {
    ...result,
    valid_from_local: grant.from,
    valid_until_local: grant.until,
  };
}
async function saveGrant() {
  await act(async () => {
    const result = grant.id
      ? await api<any>(
          `/schedule/autonomy-grants/${grant.id}`,
          "PATCH",
          { confirmed: grant.confirmed, policy: grantPolicy() },
          grant.revision,
        )
      : await api<any>("/schedule/autonomy-grants", "POST", {
          confirmed: grant.confirmed,
          policy: grantPolicy(),
        });
    grant.id = result.id;
    grant.revision = result.revision;
    grant.confirmed = false;
    grantBase.value = JSON.stringify(grant);
    await load();
    notice.value = "自主范围已保存；只有范围内的活动可以自动安排。";
    emit("changed");
  });
}
function editGrant(item: any) {
  if (
    JSON.stringify(grant) !== grantBase.value &&
    !confirm("载入此授权将放弃当前授权表单，是否继续？")
  )
    return;
  for (const key of Object.keys(grant))
    if (key in item.policy) grant[key] = structuredClone(item.policy[key]);
  Object.assign(grant, {
    id: item.id,
    revision: item.revision,
    from: item.local_from,
    until: item.local_until,
    confirmed: false,
  });
  grantBase.value = JSON.stringify(grant);
}
async function revokeGrant(item: any) {
  if (!confirm(`撤销“${item.policy.title}”的自主范围？已消费的预算保留。`))
    return;
  await act(async () => {
    await api(
      `/schedule/autonomy-grants/${item.id}/revoke`,
      "POST",
      {},
      item.revision,
    );
    await load();
    notice.value = "自主范围已撤销，旧请求需停止并重新核对。";
  });
}
function planBody() {
  return {
    definition: {
      goal: plan.goal,
      grant_id: plan.grant_id,
      resource_ids: plan.resource_ids,
      duration_minutes: plan.duration_minutes,
      planning_mode: plan.planning_mode,
    },
    state: plan.enabled ? "active" : "paused",
  };
}
async function savePlan() {
  await act(async () => {
    const result = plan.id
      ? await api<any>(
          `/schedule/plans/${plan.id}`,
          "PATCH",
          planBody(),
          plan.revision,
        )
      : await api<any>("/schedule/plans", "POST", planBody());
    plan.id = result.id;
    plan.revision = result.revision;
    planBase.value = JSON.stringify(plan);
    await load();
    notice.value = plan.enabled
      ? "目标已启用，后续安排受现有范围、冲突和预算限制。"
      : "工作目标已保存为暂停状态。";
    emit("changed");
  });
}
function editPlan(item: any) {
  if (
    JSON.stringify(plan) !== planBase.value &&
    !confirm("载入此目标将放弃当前目标表单，是否继续？")
  )
    return;
  Object.assign(plan, {
    ...item.definition,
    id: item.id,
    revision: item.revision,
    enabled: item.state === "active" || item.state === "blocked",
  });
  planBase.value = JSON.stringify(plan);
}
async function previewPlan(item: any) {
  await act(async () => {
    planPreview.value = await api(`/schedule/plans/${item.id}/preview`);
  });
}
onMounted(() => void load().catch((error) => emit("error", error)));
</script>
<template>
  <section class="panel feature-section schedule-work">
    <div class="section-heading">
      <div>
        <h2>授权阅读与工作目标</h2>
        <p>
          只有明确提供的私人文本可被读取。覆盖按成功提交的片段计算，不把排程或模型自述当作已读。
        </p>
      </div>
      <button class="secondary" :disabled="busy" @click="act(load)">
        刷新书架与目标
      </button>
    </div>
    <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
    <details>
      <summary>授权文本与阅读进度</summary>
      <form @submit.prevent="saveBook">
        <fieldset :disabled="busy">
          <label
            >授权文本标题<input
              v-model="book.title"
              maxlength="240"
              required /></label
          ><label
            >选择本地UTF-8文本<input
              type="file"
              accept=".txt,text/plain"
              @change="file" /></label
          ><label
            >授权文本正文<textarea
              v-model="book.content"
              rows="5"
              required
              maxlength="524288"
            ></textarea></label
          ><label class="checkbox"
            ><input
              v-model="book.confirmed"
              type="checkbox"
              required
            />明确允许当前模型服务阅读这份私人文本</label
          >
          <p class="muted">
            最多512KiB；不会读取服务器路径、下载文本里的链接或自动写入长期记忆。
          </p>
          <button class="primary">保存并授权文本</button>
        </fieldset>
      </form>
      <article v-for="item in books" :key="item.id" class="job-card">
        <strong>{{ item.title }}</strong>
        <p>
          {{
            item.revoked_at
              ? "已撤销"
              : `实际覆盖 ${item.progress?.checkpoint ?? 0} / ${item.characters} 字符`
          }}
        </p>
        <div class="form-actions">
          <button class="secondary" :disabled="busy" @click="showBook(item)">
            查看覆盖与笔记</button
          ><button
            v-if="!item.revoked_at"
            class="secondary danger"
            :disabled="busy"
            @click="revokeBook(item)"
          >
            撤销文本授权
          </button>
        </div>
      </article>
      <article v-if="bookDetail" class="job-card">
        <h3>{{ bookDetail.title }}的阅读记录</h3>
        <p v-if="!bookDetail.notes.length" class="muted">尚无可用笔记。</p>
        <p class="muted">
          笔记为模型输出；实际覆盖只以宿主保存的字符范围为准。
        </p>
        <article v-for="(note, index) in bookDetail.notes" :key="index">
          <p>
            第 {{ note.generation }} 次阅读 · 字符 {{ note.start_char }}–{{
              note.end_char
            }}
            · {{ note.model }}
          </p>
          <pre class="preview">{{ note.content }}</pre>
        </article>
      </article>
    </details>
    <details>
      <summary>自主安排范围</summary>
      <p class="muted">
        人工授予类型、资料、时间窗与预算。超范围候选须单独批准，暂停或取消不返还已经占用的额度。
      </p>
      <form @submit.prevent="saveGrant">
        <fieldset :disabled="busy">
          <label
            >授权名称<input
              v-model="grant.title"
              maxlength="240"
              required /></label
          ><label
            >授权时区<input v-model="grant.time_zone" maxlength="128" required
          /></label>
          <div class="form-row">
            <label
              >授权生效<input
                v-model="grant.from"
                type="datetime-local"
                step="1"
                required /></label
            ><label
              >授权截止<input
                v-model="grant.until"
                type="datetime-local"
                step="1"
                required
            /></label>
          </div>
          <div class="form-row">
            <label
              >每日可用起点<input
                v-model="grant.window_start"
                type="time"
                required /></label
            ><label
              >每日可用终点<input
                v-model="grant.window_end"
                type="time"
                required
            /></label>
          </div>
          <p>允许的类型</p>
          <label
            v-for="[key, label] in [
              ['reading', '文本阅读'],
              ['writing', '文字活动'],
              ['video', '视频学习'],
              ['consolidation', '整理'],
              ['chat', '主动联系'],
              ['action', '动作'],
            ]"
            :key="key"
            class="checkbox"
            ><input
              v-model="grant.allowed_types"
              :value="key"
              type="checkbox"
            />允许{{ label }}的自主安排</label
          >
          <p>授权书架</p>
          <label v-for="item in activeBooks" :key="item.id" class="checkbox"
            ><input
              v-model="grant.resource_ids"
              type="checkbox"
              :value="item.id"
            />范围内可用：{{ item.title }}</label
          >
          <div class="form-row">
            <label
              v-for="[key, label, min, max] in [
                ['horizon_days', '规划范围（天）', 1, 90],
                ['max_daily_activities', '每日活动上限', 1, 24],
                ['max_daily_minutes', '每日活动分钟上限', 1, 480],
                ['max_daily_changes', '每日自主变更上限', 1, 100],
              ]"
              :key="key"
              >{{ label
              }}<input
                v-model.number="grant[key]"
                type="number"
                :min="min"
                :max="max"
                required
            /></label>
          </div>
          <details>
            <summary>模型请求预算</summary>
            <div class="form-row">
              <label
                v-for="[key, label, min, max] in [
                  ['max_daily_model_calls', '每日请求上限', 1, 48],
                  ['max_daily_tokens', '每日保守预留预算', 4000, 250000],
                  ['max_output_tokens', '每次输出上限', 256, 2000],
                ]"
                :key="key"
                >{{ label
                }}<input
                  v-model.number="grant[key]"
                  type="number"
                  :min="min"
                  :max="max"
                  required
              /></label>
            </div>
            <p class="muted">
              请求失败、取消或消耗不确定时不返还预留；此数不是模型服务的实际账单。仍受实例共享并发与总预算约束。
            </p>
          </details>
          <label class="checkbox"
            ><input
              v-model="grant.confirmed"
              type="checkbox"
              required
            />明确授予以上范围，不授予消息发送或程序执行权限</label
          ><button class="primary">
            {{ grant.id ? "保存授权新版本" : "保存自主授权" }}
          </button>
        </fieldset>
      </form>
      <article v-for="item in grants" :key="item.id" class="job-card">
        <strong>{{ item.policy.title }}</strong>
        <p>
          版本 {{ item.revision }} ·
          {{ item.revoked_at ? "已撤销" : "已保存" }} ·
          {{ item.policy.time_zone }}
        </p>
        <div class="form-actions">
          <button class="secondary" :disabled="busy" @click="editGrant(item)">
            载入授权表单</button
          ><button
            v-if="!item.revoked_at"
            class="secondary danger"
            :disabled="busy"
            @click="revokeGrant(item)"
          >
            撤销自主范围
          </button>
        </div>
      </article>
    </details>
    <details>
      <summary>工作目标与规划</summary>
      <form @submit.prevent="savePlan">
        <fieldset :disabled="busy">
          <label
            >工作目标<textarea
              v-model="plan.goal"
              maxlength="4000"
              rows="2"
              required
            ></textarea></label
          ><label
            >采用的自主范围<select
              v-model="plan.grant_id"
              aria-label="工作目标授权"
              required
            >
              <option value="" disabled>选择已授予的范围</option>
              <option
                v-for="item in grants.filter((g) => !g.revoked_at)"
                :key="item.id"
                :value="item.id"
              >
                {{ item.policy.title }}
              </option>
            </select></label
          >
          <p>目标使用的资料</p>
          <label v-for="item in planBooks" :key="item.id" class="checkbox"
            ><input
              v-model="plan.resource_ids"
              type="checkbox"
              :value="item.id"
            />目标采用：{{ item.title }}</label
          ><label
            >单次阅读分钟数<input
              v-model.number="plan.duration_minutes"
              type="number"
              min="1"
              max="480"
              required /></label
          ><label
            >规划方式<select
              v-model="plan.planning_mode"
              aria-label="工作目标规划方式"
            >
              <option value="bounded_rule">按规则选择授权空闲段</option>
              <option value="model">模型选择已验证候选时段</option>
            </select></label
          >
          <p v-if="plan.planning_mode === 'model'" class="muted">
            模型规划会将本目标和授权资料标题发送给当前模型服务；不会发送其他账户的标题或最近聊天。
          </p>
          <label class="checkbox"
            ><input
              v-model="plan.enabled"
              type="checkbox"
            />启用目标，在已有范围内自动安排</label
          ><button class="primary">保存工作目标</button>
        </fieldset>
      </form>
      <article v-for="item in plans" :key="item.id" class="job-card">
        <strong>{{ item.definition.goal }}</strong>
        <p>
          {{ scheduleStatus(item.state) }} ·
          {{ scheduleReason(item.block_reason) }}
        </p>
        <div class="form-actions">
          <button class="secondary" :disabled="busy" @click="editPlan(item)">
            载入目标表单</button
          ><button
            class="secondary"
            :disabled="busy"
            @click="previewPlan(item)"
          >
            预演目标空闲段
          </button>
        </div>
        <details>
          <summary>规划请求记录</summary>
          <p v-if="!item.planning_diagnostics.attempts.length" class="muted">
            尚无模型请求；按规则安排不会调用规划模型。
          </p>
          <p
            v-for="attempt in item.planning_diagnostics.attempts"
            :key="attempt.id"
          >
            {{ scheduleStatus(attempt.status) }} ·
            {{ scheduleReason(attempt.error_code) }} · 已预留
            {{ attempt.reserved_tokens }} 预算
          </p>
        </details>
      </article>
      <article v-if="planPreview" class="job-card">
        <strong>只读候选时段 · {{ planPreview.time_zone }}</strong>
        <p v-if="!planPreview.slots.length">当前没有授权范围内的空闲候选。</p>
        <p v-for="item in planPreview.slots" :key="item.id">
          {{
            new Intl.DateTimeFormat("zh-CN", {
              timeZone: planPreview.time_zone,
              dateStyle: "short",
              timeStyle: "short",
            }).format(new Date(item.start_at * 1000))
          }}
        </p>
      </article>
    </details>
  </section>
</template>
