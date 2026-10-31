<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api } from "../api";
import { scheduleReason } from "../schedule-labels";
const eventLabels: Record<string, string> = {
    sleep: "入睡",
    wake: "起床",
    reading: "阅读",
    writing: "文字安排",
  },
  choiceLabels: Record<string, string> = {
    execute: "如期",
    cancel: "取消本次",
    postpone: "推迟",
  },
  decisionLabels: Record<string, string> = {
    pending: "等待通知",
    running: "模型核对中",
    completed: "选择已受理",
    failed: "核对失败，默认如期",
    interrupted: "请求中断，未重复",
    skipped: "未派发",
  },
  attemptLabels: Record<string, string> = {
    completed: "已结束",
    failed: "失败",
    interrupted: "已中断",
    invalidated: "已失效",
    running: "进行中",
  };
const emit = defineEmits<{ error: [unknown] }>(),
  policy = ref<any>(null),
  status = ref<any>(null),
  decisions = ref<any[]>([]),
  busy = ref(false),
  notice = ref("");
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
  [policy.value, status.value] = await Promise.all([
    api("/rest/policy"),
    api("/rest/status"),
  ]);
  policy.value.value.anticipation_enabled ??= false;
  await loadDecisions();
}
async function loadDecisions() {
  decisions.value = (await api<any>("/schedule/decisions")).items;
}
async function save() {
  await task(async () => {
    policy.value = await api(
      "/rest/policy",
      "PUT",
      policy.value.value,
      policy.value.revision,
    );
    notice.value = "作息策略已保存；自然请求仍需独立来源控制许可。";
  });
}
async function control(action: string) {
  await task(async () => {
    status.value = await api(
      "/rest/control",
      "POST",
      { action, request_id: crypto.randomUUID() },
      status.value.state.revision,
    );
    notice.value =
      action === "sleep"
        ? "入睡请求已受理，接收继续，原唤醒期限已固定。"
        : "唤醒已受理。";
  });
}
onMounted(() => task(load));
</script>
<template>
  <section v-if="policy && status" class="panel feature-section form-panel">
    <div class="section-heading">
      <div>
        <h2>作息与睡眠</h2>
        <p>接收不中断，保留原唤醒期限；模型整理继续遵守来源出域许可。</p>
      </div>
      <span class="badge">{{
        {
          awake: "清醒",
          draining: "等待请求收尾",
          consolidating: "有界整理",
          asleep: "睡眠中",
        }[status.state.phase as string] || status.state.phase
      }}</span>
    </div>
    <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
    <p v-if="status.state.wake_at" class="muted">
      当前唤醒期限：{{
        new Date(status.state.wake_at * 1000).toLocaleString("zh-CN", {
          timeZone: "Asia/Shanghai",
        })
      }}
    </p>
    <p v-if="status.state.absolute_wake_at" class="muted">
      绝对最长睡眠期限：{{
        new Date(status.state.absolute_wake_at * 1000).toLocaleString("zh-CN", {
          timeZone: "Asia/Shanghai",
        })
      }}
    </p>
    <label class="checkbox"
      ><input
        v-model="policy.value.schedule_enabled"
        type="checkbox"
      />启用上海时间定时作息</label
    ><label class="checkbox"
      ><input
        v-model="policy.value.anticipation_enabled"
        type="checkbox"
      />提前一分钟由模型选择如期、取消本次或推迟</label
    >
    <p class="muted">
      只作用于已授权、可移动的自主阅读和文字安排，以及已启用的作息。每次推迟1–30分钟、最多两次；失败默认如期，取消起床仍保留最长睡眠期限，不发送渠道消息。
    </p>
    <label class="checkbox"
      ><input
        v-model="policy.value.natural_enabled"
        type="checkbox"
      />允许已获来源控制许可的请求</label
    ><label class="checkbox"
      ><input
        v-model="policy.value.reset_context_on_sleep"
        type="checkbox"
      />收尾后重置短期上下文，保留历史和已生成待发结果</label
    >
    <div class="form-row">
      <label
        >入睡时间<input v-model="policy.value.sleep_local" type="time" /></label
      ><label
        >起床时间<input v-model="policy.value.wake_local" type="time" /></label
      ><label
        >默认睡眠秒数<input
          v-model.number="policy.value.default_sleep_seconds"
          type="number"
          min="1"
          max="86400"
      /></label>
    </div>
    <div class="form-row">
      <label
        >最长睡眠秒数<input
          v-model.number="policy.value.max_sleep_seconds"
          type="number"
          min="60"
          max="86400" /></label
      ><label
        >请求收尾秒数<input
          v-model.number="policy.value.drain_seconds"
          type="number"
          min="0"
          max="60" /></label
      ><label
        >有界整理秒数<input
          v-model.number="policy.value.consolidation_seconds"
          type="number"
          min="1"
          max="1800"
      /></label>
    </div>
    <div class="form-actions">
      <button class="secondary" :disabled="busy" @click="task(load)">
        刷新作息状态</button
      ><button
        class="secondary"
        :disabled="
          busy ||
          status.state.phase !== 'awake' ||
          !status.clock.trusted_for_new_sleep
        "
        @click="control('sleep')"
      >
        现在请求入睡</button
      ><button
        class="secondary"
        :disabled="busy || status.state.phase === 'awake'"
        @click="control('wake')"
      >
        独立唤醒</button
      ><button class="primary" :disabled="busy" @click="save">
        保存作息策略
      </button>
    </div>
    <details aria-label="提前选择记录">
      <summary>提前选择记录</summary>
      <p class="muted">
        最近50项本人管理的通知。选择成功不代表活动已经执行；模型失败、中断或未获得槽位时仍按原计划处理。
      </p>
      <button class="secondary" :disabled="busy" @click="task(loadDecisions)">
        刷新选择记录
      </button>
      <p v-if="!decisions.length">尚无提前选择记录。</p>
      <article v-for="item in decisions" :key="item.id" class="job-card">
        <strong>{{ eventLabels[item.kind] ?? item.kind }}</strong>
        <p>
          本次期限：{{
            new Date(item.due_at * 1000).toLocaleString("zh-CN", {
              timeZone: "Asia/Shanghai",
            })
          }}
          · 已推迟{{ item.postponements }}次
        </p>
        <p>
          选择：{{ choiceLabels[item.decision] }} ·
          {{ decisionLabels[item.decision_status] ?? item.decision_status }}
        </p>
        <p v-if="item.error_code" class="muted">
          {{ scheduleReason(item.error_code) }}
        </p>
        <p v-for="attempt in item.attempts" :key="attempt.id" class="muted">
          请求{{ attemptLabels[attempt.status] ?? attempt.status }} · 预留预算{{
            attempt.reserved_tokens
          }}
        </p>
      </article>
    </details>
  </section>
</template>
