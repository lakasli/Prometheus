<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>();
const sources = ref<any[]>([]),
  source = ref(""),
  messages = ref<any[]>([]),
  selected = ref<string[]>([]),
  jobs = ref<any[]>([]),
  busy = ref(false),
  kind = ref("memory_candidates"),
  notice = ref(""),
  policy = ref<any>(null),
  grant = ref<any>(null),
  date = ref(
    new Intl.DateTimeFormat("en-CA", { timeZone: "Asia/Shanghai" }).format(
      new Date(),
    ),
  ),
  period = ref("day");
const selectable = (message: any) =>
  message.eligible !== false &&
  (kind.value !== "summary" || message.summary_eligible !== false);
watch(kind, () => {
  selected.value = selected.value.filter((id) => {
    const m = messages.value.find((m) => m.id === id);
    return m && selectable(m);
  });
});
async function task(action: () => Promise<void>) {
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
    api<any>("/knowledge/sources"),
    api<any>("/knowledge/jobs"),
    api<any>("/knowledge/policy"),
  ]);
  sources.value = a.items;
  jobs.value = b.items;
  policy.value = c;
  if (source.value) await loadSource();
}
async function loadSource() {
  const item = sources.value.find((item) => item.source_key === source.value);
  if (!item) return;
  messages.value = (
    await api<any>(
      `/knowledge/source/messages?source_key=${encodeURIComponent(source.value)}`,
    )
  ).items;
  selected.value = [];
  grant.value = {
    capture: item.capture,
    allow_model: item.allow_model,
    can_sleep: item.can_sleep,
    can_wake: item.can_wake,
    expires_at: item.expires_at ?? null,
  };
}
async function saveGrant() {
  await task(async () => {
    const item = sources.value.find((item) => item.source_key === source.value);
    await api(
      "/knowledge/source/grant",
      "PUT",
      { source_key: source.value, grant: grant.value },
      item.revision,
    );
    await load();
    notice.value = "来源授权已更新。旧待审结果失效，新增采集只覆盖之后的互动。";
  });
}
async function savePolicy() {
  await task(async () => {
    policy.value = await api(
      "/knowledge/policy",
      "PUT",
      policy.value.value,
      policy.value.revision,
    );
    notice.value = "后台整理策略已保存；仍逐项遵守来源许可。";
  });
}
async function create() {
  await task(async () => {
    await api("/knowledge/jobs", "POST", {
      kind: kind.value,
      sources: selected.value.map((id) => ({ type: "message", id })),
      options:
        kind.value === "diary"
          ? { date: date.value, period: period.value }
          : {},
      idempotency_key: crypto.randomUUID(),
    });
    await load();
    notice.value = "整理已进入队列；生成、审核与保存分别进行。";
  });
}
async function review(job: any, approve: boolean) {
  await task(async () => {
    await api(
      `/knowledge/jobs/${job.id}/review`,
      "POST",
      { approve },
      job.revision,
    );
    await load();
    notice.value = approve
      ? "结果已审核保存为私有资料。"
      : "结果已拒绝，候选正文已清除。";
  });
}
async function cancel(job: any) {
  await task(async () => {
    await api(`/knowledge/jobs/${job.id}`, "DELETE", undefined, job.revision);
    await load();
  });
}
async function retry(job: any) {
  await task(async () => {
    await api(`/knowledge/jobs/${job.id}/retry`, "POST", {}, job.revision);
    await load();
  });
}
const statuses: Record<string, string> = {
  queued: "排队中",
  running: "整理中",
  review_pending: "待审核",
  approved: "已审核保存",
  rejected: "已拒绝",
  completed: "已完成",
  failed: "失败",
  interrupted: "已中断",
  cancelled: "已取消",
  invalidated: "来源已失效",
};
onMounted(() => task(load));
</script>
<template>
  <div class="feature-stack">
    <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
    <section class="panel">
      <div class="section-heading">
        <div>
          <h2>来源与出域许可</h2>
          <p>采集、发送给模型与作息控制分别授权，默认关闭。</p>
        </div>
        <button class="secondary" :disabled="busy" @click="task(load)">
          刷新整理记录
        </button>
      </div>
      <label
        >会话来源<select v-model="source" @change="task(loadSource)">
          <option value="">请选择一个来源</option>
          <option
            v-for="item in sources"
            :key="item.id"
            :value="item.source_key"
          >
            {{ item.title }} · {{ item.owner_id }}
          </option>
        </select></label
      >
      <div v-if="grant" class="grant-grid">
        <label class="checkbox"
          ><input
            v-model="grant.capture"
            type="checkbox"
          />采集之后的完成互动</label
        ><label class="checkbox"
          ><input
            v-model="grant.allow_model"
            type="checkbox"
          />允许此来源发送给模型整理</label
        ><label class="checkbox"
          ><input
            v-model="grant.can_sleep"
            type="checkbox"
          />允许此来源请求入睡</label
        ><label class="checkbox"
          ><input
            v-model="grant.can_wake"
            type="checkbox"
          />允许此来源请求唤醒</label
        ><button class="secondary" :disabled="busy" @click="saveGrant">
          保存此来源授权
        </button>
      </div>
      <div v-if="messages.length" class="source-list">
        <label
          v-for="message in messages"
          :key="message.id"
          class="source-message"
          ><input
            v-model="selected"
            type="checkbox"
            :value="message.id"
            :disabled="!selectable(message)"
          /><span
            ><strong>{{ message.role === "user" ? "对方" : "刘嘉卉" }}</strong
            ><span class="record-body">{{ message.content }}</span></span
          ></label
        >
      </div>
      <div class="form-row">
        <label
          >整理方式<select v-model="kind">
            <option value="memory_candidates">
              提取记忆候选（最多12条来源）
            </option>
            <option value="summary">整理本会话摘要</option>
            <option value="diary">整理日记 / 周记 / 月记</option>
            <option value="relationships">提议人物与有向关系</option>
          </select></label
        ><template v-if="kind === 'diary'"
          ><label>日期<input v-model="date" type="date" /></label
          ><label
            >周期<select v-model="period">
              <option value="day">日记</option>
              <option value="week">周记</option>
              <option value="month">月记</option>
            </select></label
          ></template
        >
      </div>
      <button
        class="primary"
        :disabled="
          busy ||
          !selected.length ||
          (kind === 'memory_candidates' && selected.length > 12)
        "
        @click="create"
      >
        整理选中的 {{ selected.length }} 条来源
      </button>
    </section>
    <section v-if="policy" class="panel">
      <h2>后台整理</h2>
      <label class="checkbox"
        ><input
          v-model="policy.value.automatic_diary"
          type="checkbox"
        />为已授权来源生成日记待审稿</label
      ><label class="checkbox"
        ><input
          v-model="policy.value.automatic_summary"
          type="checkbox"
        />为已授权来源更新会话摘要</label
      ><label
        >每日后台模型调用上限<input
          v-model.number="policy.value.max_daily_calls"
          type="number"
          min="1"
          max="128" /></label
      ><button class="secondary" :disabled="busy" @click="savePolicy">
        保存后台整理策略
      </button>
    </section>
    <section class="panel">
      <h2>整理与审核记录</h2>
      <p v-if="!jobs.length" class="empty">选择明确来源后，再开始整理。</p>
      <article v-for="job in jobs" :key="job.id" class="job-card">
        <div class="section-heading">
          <strong>{{
            {
              memory_candidates: "记忆候选",
              summary: "会话摘要",
              diary: "日记整理",
              relationships: "人物与关系",
              schedule_plan: "日程提议",
              trigger_draft: "触发草稿",
              drawing_html: "插画",
            }[job.kind as string] || job.kind
          }}</strong
          ><span class="badge">{{ statuses[job.status] || job.status }}</span>
        </div>
        <p class="muted">
          已处理 {{ job.progress.processed }} /
          {{ job.progress.sources }} 条来源 · {{ job.progress.calls }} 次调用
        </p>
        <p v-if="job.error" class="alert error">{{ job.error }}</p>
        <div v-if="job.output">
          <p v-if="job.output.summary" class="record-body">
            {{ job.output.summary }}
          </p>
          <ul v-if="job.output.candidates">
            <li v-for="(item, index) in job.output.candidates" :key="index">
              {{ item.content }}
            </li>
          </ul>
          <template v-if="job.output.facts"
            ><h3>{{ job.output.title }}</h3>
            <p class="record-body">{{ job.output.facts }}</p>
            <p class="feeling">{{ job.output.feelings }}</p></template
          ><template v-if="job.output.people"
            ><p v-for="(person, index) in job.output.people" :key="index">
              <strong>{{ person.name }}</strong
              >：{{ person.notes }}
            </p>
            <p
              v-for="(edge, index) in job.output.relationships"
              :key="'e' + index"
            >
              {{ job.output.people[edge.from_index]?.name }} →
              {{ job.output.people[edge.to_index]?.name }}：{{
                edge.description
              }}
            </p></template
          ><iframe
            v-if="job.kind === 'drawing_html'"
            :src="`/api/admin/v1/knowledge/jobs/${job.id}/preview`"
            sandbox=""
            title="隔离的插画预览"
            class="art-preview"
          ></iframe>
        </div>
        <div class="form-actions">
          <template
            v-if="
              job.status === 'review_pending' && job.kind !== 'drawing_html'
            "
            ><button
              class="secondary"
              :disabled="busy"
              @click="review(job, false)"
            >
              拒绝并清除候选</button
            ><button
              v-if="
                [
                  'memory_candidates',
                  'diary',
                  'relationships',
                  'schedule_plan',
                ].includes(job.kind)
              "
              class="primary"
              :disabled="busy"
              @click="review(job, true)"
            >
              审核后保存私有资料
            </button></template
          ><button
            v-if="['queued', 'running'].includes(job.status)"
            class="secondary"
            :disabled="busy"
            @click="cancel(job)"
          >
            取消整理</button
          ><button
            v-if="['failed', 'interrupted'].includes(job.status)"
            class="secondary"
            :disabled="busy"
            @click="retry(job)"
          >
            核对来源后重试
          </button>
        </div>
      </article>
    </section>
  </div>
</template>
