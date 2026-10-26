<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
import { api, ApiError, errorMessage } from "../api";
import { dateKey, displayTime, label } from "../life-systems";

interface Entry {
  id: string;
  revision: number;
  period: string;
  period_start: string;
  period_end: string;
  timezone: string;
  visibility: string;
  source_owner_id: string | null;
  facts: string;
  feelings: string;
  status: string;
  generator: string;
  source_count: number;
  observation_complete: boolean;
  coverage_watermark: number;
  deletion_epoch: number;
}
interface DiaryEvent {
  id: string;
  revision: number;
  content: string;
  visibility: string;
  source_owner_id: string | null;
  origin_type: string;
  happened_at: number;
  observed_at: number;
  assertion_kind: string;
}
interface Source {
  source_owner_id: string;
  title: string;
  revision?: number;
  policy_revision?: number;
  enabled: boolean;
  allow_model: boolean;
  timezone?: string;
}
const emit = defineEmits<{ unauthorized: [] }>();
const entries = ref<Entry[]>([]),
  events = ref<DiaryEvent[]>([]),
  sources = ref<Source[]>([]);
const selected = ref<Entry | null>(null),
  evidence = ref<DiaryEvent[]>([]);
const period = ref("day"),
  query = ref(""),
  source = ref("");
const tab = ref("entries"),
  busy = ref(false),
  error = ref(""),
  notice = ref("");
const total = ref(0),
  offset = ref(0),
  selectedSource = ref("");
const writing = reactive({ content: "", shared: false });
const editing = ref<DiaryEvent | null>(null),
  editContent = ref("");
const forgetting = ref<DiaryEvent | null>(null);
const policy = reactive({ enabled: false, allow_model: false });
const consolidationDay = ref(dateKey(Date.now() / 1000));
const currentPolicy = computed(() =>
  sources.value.find((s) => s.source_owner_id === selectedSource.value),
);
const status = ref<{ pending_model_jobs?: number; deletion_epoch?: number }>(
  {},
);
let disposed = false;
const controller = new AbortController();
function ownError(e: unknown) {
  if (e instanceof ApiError && e.status === 401) emit("unauthorized");
  return e instanceof Error && !(e instanceof ApiError)
    ? e.message
    : errorMessage(e);
}
async function task(action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  notice.value = "";
  try {
    await action();
  } catch (e) {
    if (!disposed) error.value = ownError(e);
  } finally {
    if (!disposed) busy.value = false;
  }
}
async function load() {
  const params = new URLSearchParams({
    period: period.value,
    q: query.value,
    limit: "20",
    offset: String(offset.value),
  });
  if (source.value) params.set("source_owner_id", source.value);
  const [page, eventPage, sourcePage, state] = await Promise.all([
    api<{ items: Entry[]; total: number }>(`/diary/entries?${params}`, {
      signal: controller.signal,
    }),
    api<{ items: DiaryEvent[] }>(`/diary/events?${params}`, {
      signal: controller.signal,
    }),
    api<{ items: Source[] }>("/diary/sources", { signal: controller.signal }),
    api<typeof status.value>("/diary/status", { signal: controller.signal }),
  ]);
  if (disposed) return;
  entries.value = page.items;
  total.value = page.total;
  events.value = eventPage.items;
  sources.value = sourcePage.items;
  status.value = state;
  if (selected.value) {
    selected.value =
      page.items.find((e) => e.id === selected.value!.id) || null;
    if (!selected.value) evidence.value = [];
  }
}
async function inspect(entry: Entry) {
  await task(async () => {
    const result = await api<{ items: DiaryEvent[] }>(
      `/diary/entries/${entry.id}/sources`,
    );
    selected.value = entry;
    evidence.value = result.items;
  });
}
async function saveNote() {
  await task(async () => {
    await api("/diary/events", {
      method: "POST",
      body: {
        content: writing.content,
        source_conversation_id: null,
        visibility: writing.shared ? "shared" : "private",
        publish: writing.shared,
        happened_at: null,
      },
    });
    writing.content = "";
    writing.shared = false;
    await load();
    notice.value = "已记下，等待日记整理。";
  });
}
async function correct() {
  const target = editing.value;
  if (!target) return;
  await task(async () => {
    await api(`/diary/events/${encodeURIComponent(target.id)}`, {
      method: "PATCH",
      revision: target.revision,
      body: { content: editContent.value },
    });
    editing.value = null;
    selected.value = null;
    await load();
    notice.value = "更正已保存，旧日记与派生资料已失效，等待重新整理。";
  });
}
async function forget() {
  const target = forgetting.value;
  if (!target) return;
  await task(async () => {
    await api(`/diary/events/${encodeURIComponent(target.id)}`, {
      method: "DELETE",
      revision: target.revision,
    });
    forgetting.value = null;
    selected.value = null;
    await load();
    notice.value =
      "事件已遗忘，相关日记与人物派生资料已失效。原始对话和离线备份按各自保留策略处理。";
  });
}
function choosePolicy() {
  policy.enabled = currentPolicy.value?.enabled || false;
  policy.allow_model = currentPolicy.value?.allow_model || false;
}
async function savePolicy() {
  await task(async () => {
    await api("/diary/policies", {
      method: "PUT",
      revision:
        currentPolicy.value?.policy_revision ??
        currentPolicy.value?.revision ??
        0,
      body: {
        source_owner_id: selectedSource.value,
        enabled: policy.enabled,
        allow_model: policy.enabled && policy.allow_model,
        timezone: "Asia/Shanghai",
      },
    });
    await load();
    notice.value = "来源策略已保存并生效。";
  });
}
async function consolidate() {
  await task(async () => {
    const entry = await api<Entry>("/diary/consolidate", {
      method: "POST",
      body: {
        period: period.value,
        day: consolidationDay.value,
        timezone: "Asia/Shanghai",
        visibility: "private",
        source_owner_id: source.value || null,
      },
    });
    await load();
    selected.value = entry;
    notice.value = "整理结果已保存；生成方式和来源覆盖见详情。";
  });
}
function sourceName(owner: string | null) {
  return (
    sources.value.find((s) => s.source_owner_id === owner)?.title ||
    (owner ? "私有来源" : "已发布资料")
  );
}
onMounted(() => void task(load));
onUnmounted(() => {
  disposed = true;
  controller.abort();
});
</script>

<template>
  <div class="life-system diary-system">
    <p v-if="error" role="alert" class="error-banner">{{ error }}</p>
    <p v-if="notice" role="status" class="notice-banner">{{ notice }}</p>
    <section class="panel life-intro">
      <div>
        <p class="eyebrow">DAYS, STORIES & MEMORIES</p>
        <h2>把经历慢慢记下来</h2>
        <p>事实与感受分别保存。每一篇日记、周记和月记都能回到最初的来源。</p>
      </div>
      <button :disabled="busy" @click="task(load)">刷新</button>
    </section>
    <div class="life-tabs" role="tablist" aria-label="日记内容">
      <button
        v-for="item in [
          { id: 'entries', name: '日记与资料' },
          { id: 'events', name: '待整理事件' },
          { id: 'policy', name: '来源与整理' },
        ]"
        :key="item.id"
        role="tab"
        :aria-selected="tab === item.id"
        :class="{ active: tab === item.id }"
        @click="tab = item.id"
      >
        {{ item.name }}
      </button>
    </div>
    <section v-if="tab !== 'policy'" class="panel">
      <form
        class="life-toolbar"
        @submit.prevent="
          offset = 0;
          task(load);
        "
      >
        <label
          >周期<select v-model="period">
            <option value="day">日记</option>
            <option value="week">周记</option>
            <option value="month">月记</option>
          </select></label
        >
        <label
          >来源<select v-model="source">
            <option value="">全部有权查看的来源</option>
            <option
              v-for="s in sources"
              :key="s.source_owner_id"
              :value="s.source_owner_id"
            >
              {{ s.title }}
            </option>
          </select></label
        >
        <label class="life-grow"
          >搜索<input
            v-model="query"
            type="search"
            placeholder="查找经历、人物或约定"
            maxlength="128"
        /></label>
        <button :disabled="busy">查询</button>
      </form>
      <template v-if="tab === 'entries'">
        <div class="life-split">
          <div class="life-cards" :aria-busy="busy">
            <p v-if="!entries.length" class="life-empty">
              还没有这个范围的日记。记录事件并开启来源整理后，会在这里逐步积累。
            </p>
            <button
              v-for="entry in entries"
              :key="entry.id"
              class="life-card diary-card"
              :class="{ selected: selected?.id === entry.id }"
              @click="inspect(entry)"
            >
              <span class="life-card-top"
                ><strong
                  >{{ entry.period_start }} ·
                  {{
                    { day: "日记", week: "周记", month: "月记" }[
                      entry.period
                    ] || entry.period
                  }}</strong
                ><span class="life-badge">{{ label(entry.status) }}</span></span
              >
              <span>{{ entry.facts.slice(0, 140) || "没有新互动记录" }}</span>
              <small
                >{{ sourceName(entry.source_owner_id) }} ·
                {{ entry.source_count }} 个来源 · v{{ entry.revision }}</small
              >
            </button>
          </div>
          <article v-if="selected" class="life-detail" aria-label="日记详情">
            <h3>
              {{ selected.period_start }} 的{{
                { day: "日记", week: "周记", month: "月记" }[selected.period]
              }}
            </h3>
            <p class="life-meta">
              {{ sourceName(selected.source_owner_id) }} ·
              {{ selected.timezone }} · v{{ selected.revision }}
            </p>
            <p class="life-badge">
              {{
                selected.generator.startsWith("model:")
                  ? "模型整理"
                  : "来源摘录"
              }}
              ·
              {{
                selected.observation_complete ? "观测完整" : "存在未观测时段"
              }}
            </p>
            <h4>发生的事</h4>
            <p class="life-prose">{{ selected.facts }}</p>
            <h4>感受与想法</h4>
            <p class="life-prose">
              {{ selected.feelings || "本次没有生成主观感受。" }}
            </p>
            <details>
              <summary>来源与版本 · {{ evidence.length }} 条</summary>
              <div
                v-for="event in evidence"
                :key="event.id"
                class="life-evidence"
              >
                <p class="life-prose">{{ event.content }}</p>
                <small
                  >{{ displayTime(event.happened_at) }} ·
                  {{ event.assertion_kind }} · v{{ event.revision }}</small
                >
                <div class="life-actions">
                  <button
                    @click="
                      editing = event;
                      editContent = event.content;
                    "
                  >
                    更正</button
                  ><button @click="forgetting = event">遗忘</button>
                </div>
              </div>
              <small
                >覆盖水位 {{ selected.coverage_watermark }} · 删除水位
                {{ selected.deletion_epoch }}</small
              >
            </details>
          </article>
        </div>
        <div class="life-actions life-pagination">
          <button
            :disabled="busy || offset === 0"
            @click="
              offset -= 20;
              task(load);
            "
          >
            上一页</button
          ><span>{{ total }} 篇</span
          ><button
            :disabled="busy || offset + 20 >= total"
            @click="
              offset += 20;
              task(load);
            "
          >
            下一页
          </button>
        </div>
      </template>
      <div v-else class="life-cards">
        <p v-if="!events.length" class="life-empty">当前没有事件。</p>
        <article v-for="event in events" :key="event.id" class="life-evidence">
          <p class="life-prose">{{ event.content }}</p>
          <small
            >{{ sourceName(event.source_owner_id) }} ·
            {{ displayTime(event.happened_at) }} · v{{ event.revision }}</small
          >
          <div class="life-actions">
            <button
              @click="
                editing = event;
                editContent = event.content;
              "
            >
              更正</button
            ><button @click="forgetting = event">遗忘</button>
          </div>
        </article>
      </div>
    </section>
    <section v-if="tab === 'events'" class="panel">
      <h3>记下一件事</h3>
      <form class="life-form" @submit.prevent="saveNote">
        <label
          >内容<textarea
            v-model="writing.content"
            rows="4"
            required
            maxlength="4000"
            placeholder="今天发生了什么，或有什么值得记住的约定？"
          ></textarea></label
        ><label class="life-check"
          ><input
            v-model="writing.shared"
            type="checkbox"
          />明确发布为跨来源共享资料</label
        ><button class="primary" :disabled="busy || !writing.content.trim()">
          保存事件
        </button>
      </form>
    </section>
    <template v-if="tab === 'policy'">
      <section class="panel">
        <h3>来源授权</h3>
        <p class="life-meta">
          自动收集默认关闭。允许模型整理后，该来源的已授权内容会发给当前配置的模型；其他来源不会混入。
        </p>
        <form class="life-form" @submit.prevent="savePolicy">
          <label
            >选择来源<select v-model="selectedSource" @change="choosePolicy">
              <option value="">请选择</option>
              <option
                v-for="s in sources"
                :key="s.source_owner_id"
                :value="s.source_owner_id"
              >
                {{ s.title }}
              </option>
            </select></label
          ><label class="life-check"
            ><input
              v-model="policy.enabled"
              type="checkbox"
            />收集该来源已完成的真实互动并整理日记</label
          ><label class="life-check"
            ><input
              v-model="policy.allow_model"
              type="checkbox"
              :disabled="!policy.enabled"
            />允许当前模型整理该来源内容</label
          ><button class="primary" :disabled="busy || !selectedSource">
            保存来源策略
          </button>
        </form>
      </section>
      <section class="panel">
        <h3>立即整理</h3>
        <p class="life-meta">
          按来源分别整理，不把多个私有来源合并。无互动时明确记录没有新经历。
        </p>
        <form class="life-toolbar" @submit.prevent="consolidate">
          <label
            >周期<select v-model="period">
              <option value="day">日记</option>
              <option value="week">周记</option>
              <option value="month">月记</option>
            </select></label
          ><label
            >日期<input
              v-model="consolidationDay"
              type="date"
              required /></label
          ><label
            >来源<select v-model="source">
              <option value="">当前管理来源</option>
              <option
                v-for="s in sources"
                :key="s.source_owner_id"
                :value="s.source_owner_id"
              >
                {{ s.title }}
              </option>
            </select></label
          ><button :disabled="busy">整理并保存</button>
        </form>
      </section>
    </template>
    <section
      v-if="editing"
      class="panel life-confirm"
      role="region"
      aria-label="更正事件"
    >
      <h3>更正这件事</h3>
      <form class="life-form" @submit.prevent="correct">
        <label
          >正确内容<textarea
            v-model="editContent"
            rows="4"
            required
            maxlength="4000"
          ></textarea>
        </label>
        <p>保存后，引用旧版本的日记与人物资料会失效并等待重新整理。</p>
        <div class="life-actions">
          <button class="primary" :disabled="busy">保存更正</button
          ><button type="button" @click="editing = null">取消</button>
        </div>
      </form>
    </section>
    <section
      v-if="forgetting"
      class="panel life-confirm"
      role="region"
      aria-label="遗忘事件"
    >
      <h3>确认遗忘这件事</h3>
      <p class="life-prose">{{ forgetting.content }}</p>
      <p>这会清除事件内容，并使相关日记、人物资料和旧上下文失效。</p>
      <div class="life-actions">
        <button class="danger" :disabled="busy" @click="forget">确认遗忘</button
        ><button @click="forgetting = null">取消</button>
      </div>
    </section>
  </div>
</template>
