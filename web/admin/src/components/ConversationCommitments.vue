<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, reactive, ref } from "vue";
import { api, ApiError, errorMessage } from "../api";
import { displayTime } from "../life-systems";
import type {
  CommitmentActivity,
  CommitmentCounterparty,
  CommitmentScope,
  CommitmentSubject,
  ConversationCommitment,
} from "../context-management";

type Cursor = number | string | null;
interface Page<T> {
  items: T[];
  next_cursor?: Cursor;
}
const emit = defineEmits<{ close: []; unauthorized: [] }>();
const dialog = ref<HTMLElement | null>(null);
const scopes = ref<CommitmentScope[]>([]),
  scopeKey = ref(""),
  scopeCursor = ref<Cursor>(null);
const items = ref<ConversationCommitment[]>([]),
  itemCursor = ref<Cursor>(null);
const selected = ref<ConversationCommitment | null>(null);
const activities = ref<CommitmentActivity[]>([]),
  activityCursor = ref<Cursor>(null),
  activityKey = ref("");
const activitiesLoaded = ref(false);
const busy = ref(false),
  error = ref(""),
  notice = ref(""),
  stale = ref(false),
  createOpen = ref(false);
const confirmChecked = ref(false),
  cancelOpen = ref(false),
  cancelActivity = ref(false);
const proposal = reactive({ intent: "", kind: "reading", time: "" });
const participants = reactive<{
  subject: CommitmentSubject;
  counterparty: CommitmentCounterparty;
}>({ subject: "unresolved", counterparty: "unresolved" });
const scope = computed(() =>
  scopes.value.find((item) => key(item) === scopeKey.value),
);
const participantsDirty = computed(
  () =>
    !!selected.value &&
    (participants.subject !== selected.value.subject_kind ||
      participants.counterparty !== selected.value.counterparty_kind),
);
const hasUnsaved = computed(
  () =>
    busy.value ||
    !!proposal.intent.trim() ||
    !!proposal.time.trim() ||
    participantsDirty.value ||
    !!activityKey.value,
);
const canBind = computed(
  () =>
    !!selected.value &&
    selected.value.confirmation_state === "confirmed" &&
    ["assistant", "joint"].includes(selected.value.subject_kind) &&
    !["reminder", "conversation"].includes(selected.value.kind) &&
    selected.value.execution.state === "unscheduled",
);
const canCancel = computed(
  () =>
    !!selected.value &&
    ["proposed", "confirmed"].includes(selected.value.confirmation_state) &&
    selected.value.execution.state !== "needs_review" &&
    !["running", "unknown", "completed"].includes(
      selected.value.execution.state,
    ),
);
defineExpose({ hasUnsaved });
let disposed = false;
let controller: AbortController | undefined;
let previousFocus: HTMLElement | null = null;
const mutationKeys = new Map<string, string>();
const confirmationLabels: Record<string, string> = {
  proposed: "待确认候选",
  confirmed: "资料已审核确认",
  cancelled: "约定已取消",
  expired: "约定已到期",
};
const executionLabels: Record<string, string> = {
  unscheduled: "尚未调度",
  scheduled: "已关联真实日程",
  running: "活动执行中",
  completed: "活动有完成证据",
  failed: "活动执行失败",
  unknown: "执行结果未知",
  cancelled: "活动已取消",
  expired: "执行窗口已过",
  needs_review: "执行依据待复核",
};
const subjects: Record<CommitmentSubject, string> = {
  speaker: "当前发言者",
  assistant: "她",
  joint: "双方",
  unresolved: "主体待明确",
};
const counterparties: Record<CommitmentCounterparty, string> = {
  speaker: "当前发言者",
  assistant: "她",
  unresolved: "对象待明确",
};
function key(item: CommitmentScope) {
  return item.scope_ref || `${item.conversation_id}:${item.run_id}`;
}
function sourceLabel(item: CommitmentScope) {
  const duplicates =
    scopes.value.filter(
      (other) =>
        other.title === item.title && other.source_label === item.source_label,
    ).length > 1;
  return `${item.title} · ${item.source_label} · ${displayTime(item.updated_at)}${duplicates ? ` · 来源 ${item.scope_ref.slice(0, 8)}` : ""}`;
}
function route(suffix = "") {
  if (!scope.value) throw new Error("no_verified_scope");
  return `/conversations/${encodeURIComponent(scope.value.conversation_id)}/commitments${suffix}?run_id=${encodeURIComponent(scope.value.run_id)}`;
}
function choose(item: ConversationCommitment) {
  selected.value = item;
  participants.subject = item.subject_kind || "unresolved";
  participants.counterparty = item.counterparty_kind || "unresolved";
  confirmChecked.value = false;
  cancelOpen.value = false;
  cancelActivity.value = false;
  activities.value = [];
  activityKey.value = "";
  activityCursor.value = null;
  activitiesLoaded.value = false;
}
function requestChoice(item: ConversationCommitment) {
  if (selected.value?.id === item.id) return;
  if (
    (participantsDirty.value || activityKey.value) &&
    !window.confirm("切换约定会重置尚未保存的主体、对象或活动选择，确定继续？")
  )
    return;
  choose(item);
}
function reload() {
  if (
    (participantsDirty.value || activityKey.value) &&
    !window.confirm("重新载入会重置尚未保存的主体、对象或活动选择，确定继续？")
  )
    return;
  void perform((signal) => fetchScopes(signal));
}
function replace(item: ConversationCommitment) {
  const index = items.value.findIndex((old) => old.id === item.id);
  if (index < 0) items.value.unshift(item);
  else items.value[index] = item;
  choose(item);
}
function failure(caught: unknown, writing: boolean) {
  if (caught instanceof ApiError && caught.status === 401) emit("unauthorized");
  if (caught instanceof ApiError && [403, 404].includes(caught.status)) {
    stale.value = true;
    items.value = [];
    selected.value = null;
    activities.value = [];
    error.value =
      "来源或约定已失效，或当前账号无权读取。请重新载入可访问的会话来源。";
  } else if (caught instanceof ApiError && caught.status === 409) {
    stale.value = true;
    error.value =
      "约定、日程或来源版本已变化。你的填写仍保留，请重新载入后核对。";
  } else if (
    writing &&
    (!(caught instanceof ApiError) || caught.status >= 500)
  ) {
    stale.value = true;
    error.value = "提交结果尚未确认，请先刷新核对；当前操作不会自动重发。";
  } else error.value = errorMessage(caught);
}
async function perform(
  action: (signal: AbortSignal) => Promise<void>,
  writing = false,
) {
  if (busy.value || disposed) return;
  busy.value = true;
  error.value = "";
  notice.value = "";
  controller = new AbortController();
  try {
    await action(controller.signal);
  } catch (caught) {
    if (!disposed) failure(caught, writing);
  } finally {
    if (!disposed) busy.value = false;
  }
}
async function fetchItems(signal: AbortSignal, append = false) {
  if (!scope.value) return;
  const result = await api<Page<ConversationCommitment>>(
    `${route()}&limit=20${append && itemCursor.value !== null ? `&after=${encodeURIComponent(String(itemCursor.value))}` : ""}`,
    { signal },
  );
  if (disposed) return;
  items.value = append
    ? [
        ...items.value,
        ...result.items.filter(
          (item) => !items.value.some((old) => old.id === item.id),
        ),
      ]
    : result.items;
  itemCursor.value = result.next_cursor ?? null;
  stale.value = false;
  if (!append) {
    selected.value = null;
    if (items.value[0]) choose(items.value[0]);
  }
}
async function fetchScopes(signal: AbortSignal, append = false) {
  const result = await api<Page<CommitmentScope>>(
    `/commitments/scopes?limit=20${append && scopeCursor.value !== null ? `&after=${encodeURIComponent(String(scopeCursor.value))}` : ""}`,
    { signal },
  );
  if (disposed) return;
  scopes.value = append
    ? [
        ...scopes.value,
        ...result.items.filter(
          (item) => !scopes.value.some((old) => key(old) === key(item)),
        ),
      ]
    : result.items;
  scopeCursor.value = result.next_cursor ?? null;
  if (!scopes.value.some((item) => key(item) === scopeKey.value))
    scopeKey.value = scopes.value[0] ? key(scopes.value[0]) : "";
  if (!append && scope.value) await fetchItems(signal);
}
function switchScope(event: Event) {
  const next = (event.target as HTMLSelectElement).value;
  if (
    hasUnsaved.value &&
    !window.confirm("切换会话来源会放弃尚未保存的约定填写，确定继续？")
  ) {
    (event.target as HTMLSelectElement).value = scopeKey.value;
    return;
  }
  scopeKey.value = next;
  selected.value = null;
  items.value = [];
  activities.value = [];
  activityKey.value = "";
  proposal.intent = "";
  proposal.time = "";
  mutationKeys.clear();
  void perform((signal) => fetchItems(signal));
}
function idempotency(operation: string, body: unknown, revision = 0) {
  const identity = `${scopeKey.value}:${selected.value?.id ?? "new"}:${operation}:${revision}:${JSON.stringify(body)}`;
  if (!mutationKeys.has(identity)) {
    if (mutationKeys.size >= 32)
      mutationKeys.delete(mutationKeys.keys().next().value!);
    mutationKeys.set(identity, crypto.randomUUID());
  }
  return mutationKeys.get(identity)!;
}
async function createCandidate() {
  if (!scope.value || !proposal.intent.trim() || stale.value) return;
  await perform(async (signal) => {
    const body = {
      intent: proposal.intent.trim(),
      kind: proposal.kind,
      time_text: proposal.time.trim() || null,
      expires_at: null,
      sources: [],
    };
    const item = await api<ConversationCommitment>(route(), {
      method: "POST",
      body,
      idempotencyKey: idempotency("propose", body),
      signal,
    });
    if (disposed) return;
    replace(item);
    proposal.intent = "";
    proposal.time = "";
    createOpen.value = false;
    notice.value =
      item.confirmation_state === "proposed" &&
      item.execution.state === "unscheduled"
        ? "候选已登记，尚未确认，也未创建日程。"
        : `登记结果已核对：${confirmationLabels[item.confirmation_state] || item.confirmation_state}，${executionLabels[item.execution.state] || item.execution.state}。`;
  }, true);
}
async function mutate(
  operation: "participants" | "confirm" | "activity" | "cancel",
  body: unknown,
  message: string,
) {
  const item = selected.value;
  if (!item || stale.value) return;
  await perform(async (signal) => {
    const result = await api<ConversationCommitment>(
      route(`/${encodeURIComponent(item.id)}/${operation}`),
      {
        method: "POST",
        revision: item.revision,
        idempotencyKey: idempotency(operation, body, item.revision),
        body,
        signal,
      },
    );
    if (disposed) return;
    replace(result);
    notice.value = message;
  }, true);
}
async function loadActivities(append = false) {
  if (!scope.value || !canBind.value || stale.value) return;
  await perform(async (signal) => {
    const result = await api<Page<CommitmentActivity>>(
      `/conversations/${encodeURIComponent(scope.value!.conversation_id)}/commitment-activities?run_id=${encodeURIComponent(scope.value!.run_id)}&limit=20${append && activityCursor.value !== null ? `&after=${encodeURIComponent(String(activityCursor.value))}` : ""}`,
      { signal },
    );
    if (disposed) return;
    activities.value = append
      ? [...activities.value, ...result.items]
      : result.items;
    activitiesLoaded.value = true;
    activityCursor.value = result.next_cursor ?? null;
  });
}
async function bindActivity() {
  const activity = activities.value.find(
    (item) => item.activity_id === activityKey.value,
  );
  if (!activity?.binding_supported || !canBind.value) return;
  await mutate(
    "activity",
    {
      activity_id: activity.activity_id,
      activity_revision: activity.revision,
      occurrence_id: null,
    },
    "已关联已有日程；实际执行结果与消息投递仍分别核对。",
  );
}
function close() {
  if (
    busy.value ||
    (hasUnsaved.value && !window.confirm("约定中有尚未保存的填写，确定关闭？"))
  )
    return;
  emit("close");
}
function keyboard(event: KeyboardEvent) {
  if (event.key === "Escape") {
    event.preventDefault();
    close();
  }
  if (event.key !== "Tab" || !dialog.value) return;
  const controls = [
    ...dialog.value.querySelectorAll<HTMLElement>(
      'button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex="0"]',
    ),
  ];
  const first = controls[0],
    last = controls.at(-1);
  if (
    event.shiftKey &&
    (document.activeElement === first ||
      document.activeElement === dialog.value)
  ) {
    event.preventDefault();
    last?.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first?.focus();
  }
}
onMounted(() => {
  previousFocus =
    document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
  void nextTick(() => dialog.value?.focus());
  void perform((signal) => fetchScopes(signal));
});
onUnmounted(() => {
  disposed = true;
  controller?.abort();
  if (previousFocus?.isConnected) previousFocus.focus();
});
</script>

<template>
  <div class="commitments-backdrop" @click.self="close">
    <section
      ref="dialog"
      class="commitments-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="commitments-title"
      tabindex="-1"
      @keydown="keyboard"
    >
      <header>
        <div>
          <h2 id="commitments-title">会话约定</h2>
          <p>把候选、确认、实际日程、执行与投递分别核对。</p>
        </div>
        <button
          type="button"
          aria-label="关闭会话约定"
          :disabled="busy"
          @click="close"
        >
          关闭
        </button>
      </header>
      <p v-if="error" class="error-banner" role="alert">{{ error }}</p>
      <p v-if="notice" class="notice-banner" role="status">{{ notice }}</p>
      <div class="commitments-source">
        <label class="field"
          ><span>会话来源</span
          ><select
            :value="scopeKey"
            aria-label="约定来源"
            :disabled="busy"
            @change="switchScope"
          >
            <option v-if="!scopes.length" value="">暂无可访问的会话来源</option>
            <option v-for="item in scopes" :key="key(item)" :value="key(item)">
              {{ sourceLabel(item) }}
            </option>
          </select></label
        ><button :disabled="busy" @click="reload">重新载入来源与约定</button
        ><button
          v-if="scopeCursor !== null"
          :disabled="busy"
          @click="perform((signal) => fetchScopes(signal, true))"
        >
          更多会话来源
        </button>
      </div>
      <p v-if="!scopes.length && !busy" class="muted">
        这里只列出已有可信来源的会话。选择会话即可查看约定。
      </p>
      <template v-if="scope">
        <div class="commitment-layout">
          <div class="commitment-list">
            <p v-if="!items.length" class="muted">该会话尚无可访问的约定。</p>
            <button
              v-for="item in items"
              :key="item.id"
              type="button"
              class="commitment-choice"
              :aria-pressed="selected?.id === item.id"
              :disabled="busy"
              @click="requestChoice(item)"
            >
              <strong>{{ item.intent }}</strong
              ><small
                >{{
                  confirmationLabels[item.confirmation_state] ||
                  item.confirmation_state
                }}
                ·
                {{
                  executionLabels[item.execution.state] || item.execution.state
                }}</small
              ></button
            ><button
              v-if="itemCursor !== null"
              :disabled="busy"
              @click="perform((signal) => fetchItems(signal, true))"
            >
              更多约定
            </button>
          </div>
          <article
            v-if="selected"
            class="commitment-detail"
            aria-label="约定详情"
          >
            <h3>{{ selected.intent }}</h3>
            <dl>
              <div>
                <dt>确认状态</dt>
                <dd>
                  {{
                    confirmationLabels[selected.confirmation_state] ||
                    selected.confirmation_state
                  }}
                  · v{{ selected.revision }}
                </dd>
              </div>
              <div>
                <dt>履行主体</dt>
                <dd>{{ subjects[selected.subject_kind] || "主体待明确" }}</dd>
              </div>
              <div>
                <dt>约定对象</dt>
                <dd>
                  {{
                    counterparties[selected.counterparty_kind] || "对象待明确"
                  }}
                </dd>
              </div>
              <div>
                <dt>原始时间表达</dt>
                <dd>
                  {{ selected.time.original_text || "未指定"
                  }}<span v-if="selected.time.precision === 'unresolved'">
                    · 具体时刻待明确</span
                  >
                </dd>
              </div>
              <div>
                <dt>实际执行</dt>
                <dd>
                  {{
                    executionLabels[selected.execution.state] ||
                    selected.execution.state
                  }}
                </dd>
              </div>
              <div>
                <dt>消息投递</dt>
                <dd>未提供独立投递证明；确认或关联日程不代表消息已送达。</dd>
              </div>
            </dl>
            <p v-if="selected.confirmation_state === 'confirmed'" class="notice-banner">
              <template v-if="selected.confirmation_evidence?.basis === 'authenticated_management_review'">
                这是当前管理员对约定资料的审核；未核验来源发言者或接收者同意。
              </template>
              <template v-else>确认依据待核对，不能据此认定来源发言者或接收者同意。</template>
              资料确认不代表已建立日程或获得发送许可。
            </p>
            <p
              v-if="['reminder', 'conversation'].includes(selected.kind)"
              class="notice-banner"
            >
              真实提醒与主动联系尚未开放；当前约定不会因此取得接收许可或触发外发。
            </p>
            <p
              v-if="selected.execution.state === 'unknown'"
              class="notice-banner"
            >
              执行结果未知，保留原有证据，不自动重放或声称已取消。
            </p>
            <template v-if="selected.confirmation_state === 'proposed'"
              ><form
                class="participant-review"
                @submit.prevent="
                  mutate(
                    'participants',
                    {
                      subject_kind: participants.subject,
                      counterparty_kind: participants.counterparty,
                    },
                    '主体与对象已记录，约定仍待确认。',
                  )
                "
              >
                <div class="field-row">
                  <label class="field"
                    ><span>谁来履行</span
                    ><select
                      v-model="participants.subject"
                      aria-label="履行主体"
                      :disabled="busy || stale"
                    >
                      <option value="unresolved">主体待明确</option>
                      <option value="speaker">当前发言者</option>
                      <option value="assistant">她</option>
                      <option value="joint">双方</option>
                    </select></label
                  ><label class="field"
                    ><span>约定对象</span
                    ><select
                      v-model="participants.counterparty"
                      aria-label="约定对象"
                      :disabled="busy || stale"
                    >
                      <option value="unresolved">对象待明确</option>
                      <option value="speaker">当前发言者</option>
                      <option value="assistant">她</option>
                    </select></label
                  >
                </div>
                <button :disabled="busy || stale || !participantsDirty">
                  保存主体与对象
                </button>
              </form>
              <label class="commitment-check"
                ><input
                  v-model="confirmChecked"
                  type="checkbox"
                  :disabled="busy || stale || participantsDirty"
                />我已核对这项约定，包括仍待明确的主体、对象与时间</label
              ><button
                class="primary"
                :disabled="
                  busy || stale || participantsDirty || !confirmChecked
                "
                @click="
                  mutate(
                    'confirm',
                    { confirmed: true },
                    '已记录当前管理员的资料审核。未核验来源发言者或接收者同意；审核本身不创建日程或授予发送许可。',
                  )
                "
              >
                确认约定
              </button></template
            >
            <section v-if="canBind" class="commitment-binding">
              <h4>关联已有活动</h4>
              <p class="muted">
                仅选择这项会话来源下已有、获授权且可绑定的活动。
              </p>
              <button :disabled="busy || stale" @click="loadActivities()">
                读取可关联活动
              </button>
              <p v-if="activitiesLoaded && !activities.length" class="muted">
                该来源暂无可关联的已有活动。
              </p>
              <label v-if="activities.length" class="field"
                ><span>已有日程活动</span
                ><select
                  v-model="activityKey"
                  aria-label="可关联日程活动"
                  :disabled="busy || stale"
                >
                  <option value="">请选择活动</option>
                  <option
                    v-for="activity in activities"
                    :key="activity.activity_id"
                    :value="activity.activity_id"
                    :disabled="!activity.binding_supported"
                  >
                    {{ activity.title }} ·
                    {{ displayTime(activity.start_at, activity.time_zone)
                    }}{{ activity.binding_supported ? "" : " · 当前不可绑定" }}
                  </option>
                </select></label
              ><button
                v-if="activityCursor !== null"
                :disabled="busy || stale"
                @click="loadActivities(true)"
              >
                更多已有活动</button
              ><button
                v-if="activities.length"
                class="primary"
                :disabled="
                  busy ||
                  stale ||
                  !activities.some(
                    (item) =>
                      item.activity_id === activityKey &&
                      item.binding_supported,
                  )
                "
                @click="bindActivity"
              >
                关联选中活动
              </button>
            </section>
            <p v-if="selected.subject_kind === 'speaker'" class="muted">
              这项约定由当前发言者履行，不会被转换为她的执行任务。
            </p>
            <div v-if="canCancel" class="commitment-cancel">
              <button
                :disabled="busy || stale"
                @click="cancelOpen = !cancelOpen"
              >
                取消约定</button
              ><template v-if="cancelOpen"
                ><label
                  v-if="selected.execution.activity_id"
                  class="commitment-check"
                  ><input
                    v-model="cancelActivity"
                    type="checkbox"
                  />一并取消这项尚未执行的日程活动</label
                ><button
                  :disabled="
                    busy ||
                    stale ||
                    (!!selected.execution.activity_id && !cancelActivity)
                  "
                  @click="
                    mutate(
                      'cancel',
                      {
                        cancel_schedule:
                          !!selected.execution.activity_id && cancelActivity,
                        expected_activity_revision:
                          selected.execution.current_activity_revision ?? null,
                      },
                      '取消结果已记录，已发生的执行或投递事实不会被抹去。',
                    )
                  "
                >
                  确认取消约定
                </button></template
              >
            </div>
          </article>
        </div>
        <div class="commitment-create">
          <button
            :disabled="busy || stale"
            :aria-expanded="createOpen"
            @click="createOpen = !createOpen"
          >
            登记约定候选
          </button>
          <form v-if="createOpen" @submit.prevent="createCandidate">
            <label class="field"
              ><span>约定事项</span
              ><textarea
                v-model="proposal.intent"
                aria-label="约定事项"
                maxlength="2000"
                rows="3"
                required
              ></textarea>
            </label>
            <div class="field-row">
              <label class="field"
                ><span>事项类型</span
                ><select v-model="proposal.kind" aria-label="约定类型">
                  <option value="reading">阅读</option>
                  <option value="reminder">提醒（外发未开放）</option>
                  <option value="conversation">联系（外发未开放）</option>
                  <option value="other">其他约定</option>
                </select></label
              ><label class="field"
                ><span>原始时间表达</span
                ><input
                  v-model="proposal.time"
                  aria-label="约定时间表达"
                  maxlength="200"
                  placeholder="例如：明天下午；留空表示未指定"
              /></label>
            </div>
            <p class="muted">
              登记只产生候选；不会自动确认、建立提醒或发送消息。
            </p>
            <button
              class="primary"
              :disabled="busy || stale || !proposal.intent.trim()"
            >
              保存候选
            </button>
          </form>
        </div>
      </template>
    </section>
  </div>
</template>

<style scoped>
.commitments-backdrop {
  position: fixed;
  inset: 0;
  z-index: 80;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 24px;
  background: #18233070;
}
.commitments-dialog {
  width: min(1000px, 100%);
  max-height: 90vh;
  overflow: auto;
  overscroll-behavior: contain;
  border-radius: 16px;
  background: var(--surface, #fff);
  padding: 24px;
  box-shadow: 0 20px 80px #14243e30;
  outline: none;
}
.commitments-dialog header {
  display: flex;
  justify-content: space-between;
  gap: 20px;
  align-items: flex-start;
}
.commitments-dialog h2 {
  margin: 0;
}
.commitments-dialog header p,
.commitments-dialog .muted {
  color: var(--muted);
  font-size: 12px;
  line-height: 1.8;
}
.commitments-source {
  display: flex;
  gap: 12px;
  align-items: end;
  flex-wrap: wrap;
  margin: 16px 0;
}
.commitments-source label {
  flex: 1;
  min-width: 200px;
  margin: 0;
}
.commitment-layout {
  display: grid;
  grid-template-columns: minmax(190px, 0.8fr) minmax(0, 1.8fr);
  gap: 20px;
}
.commitment-list {
  display: grid;
  align-content: start;
  gap: 10px;
}
.commitment-choice {
  text-align: left;
  display: grid;
  gap: 8px;
  white-space: normal;
  overflow-wrap: anywhere;
}
.commitment-choice[aria-pressed="true"] {
  border-color: var(--accent, #738aae);
  background: #f0f4fb;
}
.commitment-choice small {
  color: var(--muted);
  font-size: 11px;
}
.commitment-detail {
  border: 1px solid var(--line);
  border-radius: 12px;
  padding: 20px;
  min-width: 0;
}
.commitment-detail h3 {
  margin-top: 0;
  overflow-wrap: anywhere;
}
.commitment-detail dl {
  display: grid;
  gap: 10px;
  font-size: 12px;
  line-height: 1.7;
}
.commitment-detail dl div {
  display: grid;
  grid-template-columns: 90px minmax(0, 1fr);
  gap: 12px;
}
.commitment-detail dt {
  color: var(--muted);
}
.commitment-detail dd {
  margin: 0;
  overflow-wrap: anywhere;
}
.participant-review,
.commitment-binding,
.commitment-cancel,
.commitment-create {
  margin-top: 20px;
  border-top: 1px solid var(--line);
  padding-top: 18px;
}
.commitment-check {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  font-size: 12px;
  line-height: 1.7;
  margin: 16px 0;
}
.commitment-check input {
  flex: none;
  width: auto;
  margin-top: 4px;
}
.commitment-binding button,
.commitment-cancel button {
  margin: 0 8px 8px 0;
}
.commitment-create form {
  padding-top: 16px;
}
@media (max-width: 700px) {
  .commitments-backdrop {
    padding: 8px;
  }
  .commitments-dialog {
    padding: 18px;
    max-height: 96vh;
  }
  .commitment-layout {
    grid-template-columns: 1fr;
  }
  .commitment-list {
    max-height: 180px;
    overflow: auto;
  }
  .commitment-detail dl div {
    grid-template-columns: 74px minmax(0, 1fr);
  }
  .commitments-dialog .field-row {
    display: grid;
    grid-template-columns: 1fr;
  }
}
</style>
