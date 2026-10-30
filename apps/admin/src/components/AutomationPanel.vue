<script setup lang="ts">
import { onMounted, reactive, ref } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>();
const items = ref<any[]>([]),
  results = ref<any[]>([]),
  busy = ref(false),
  notice = ref("");
const form = reactive({
  type: "once",
  after_seconds: 60,
  seconds: 3600,
  time: "09:00",
  day_type: "all",
  event: "service_started",
  action: "text",
  title: "",
  content: "",
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
  const [a, b] = await Promise.all([
    api<any>("/automation/triggers"),
    api<any>("/automation/results"),
  ]);
  items.value = a.items;
  results.value = b.items;
}
async function create() {
  await task(async () => {
    const rule =
      form.type === "once"
        ? { type: "once", after_seconds: form.after_seconds }
        : form.type === "interval"
          ? { type: "interval", seconds: form.seconds }
          : form.type === "event"
            ? { type: "event", event: form.event }
            : { type: "calendar", time: form.time, day_type: form.day_type };
    await api("/automation/triggers", "POST", {
      rule,
      action: { type: form.action, title: form.title, content: form.content },
    });
    form.title = "";
    form.content = "";
    await load();
    notice.value = "已保存为停用草稿。预演并明确启用后才会运行。";
  });
}
async function toggle(item: any) {
  await task(async () => {
    const preview = await api<any>(`/automation/triggers/${item.id}/preview`);
    if (
      !item.enabled &&
      !window.confirm(
        `下一次：${preview.next_at ? new Date(preview.next_at * 1000).toLocaleString("zh-CN", { timeZone: "Asia/Shanghai" }) : "匹配新的事件时"}。结果只保存在认证页面，确定启用？`,
      )
    )
      return;
    await api(
      `/automation/triggers/${item.id}/enable`,
      "PUT",
      { enabled: !item.enabled, preview_digest: preview.digest },
      item.revision,
    );
    await load();
  });
}
async function remove(item: any) {
  if (!window.confirm("删除此触发规则及其结果记录？")) return;
  await task(async () => {
    await api(
      `/automation/triggers/${item.id}`,
      "DELETE",
      undefined,
      item.revision,
    );
    await load();
  });
}
onMounted(() => task(load));
</script>
<template>
  <section class="panel feature-section">
    <div class="section-heading">
      <div>
        <h2>任务触发</h2>
        <p>固定提醒、隔离文字任务与有界文本阅读。结果仅保存在这里。</p>
      </div>
      <button class="secondary" :disabled="busy" @click="task(load)">
        刷新触发任务
      </button>
    </div>
    <p v-if="notice" class="alert success" role="status">{{ notice }}</p>
    <details>
      <summary>新增触发草稿</summary>
      <form @submit.prevent="create">
        <div class="form-row">
          <label
            >触发方式<select v-model="form.type">
              <option value="once">启用后延迟</option>
              <option value="interval">固定秒间隔</option>
              <option value="calendar">上海民用时间</option>
              <option value="event">本人交互 / 启动事件</option>
            </select></label
          ><label v-if="form.type === 'once'"
            >延迟秒数<input
              v-model.number="form.after_seconds"
              type="number"
              min="1"
              required /></label
          ><label v-if="form.type === 'interval'"
            >间隔秒数<input
              v-model.number="form.seconds"
              type="number"
              min="60"
              required /></label
          ><template v-if="form.type === 'calendar'"
            ><label
              >时间<input v-model="form.time" type="time" required /></label
            ><label
              >日期类型<select v-model="form.day_type">
                <option value="all">每天</option>
                <option value="workday">中国工作日</option>
                <option value="restday">中国休息日</option>
              </select></label
            ></template
          ><label v-if="form.type === 'event'"
            >事件<select v-model="form.event">
              <option value="service_started">服务启动</option>
              <option value="run_completed">本人交互完成</option>
              <option value="run_failed">本人交互失败</option>
            </select></label
          >
        </div>
        <label
          >执行方式<select v-model="form.action">
            <option value="text">保存固定提醒</option>
            <option value="model_text">隔离文字模型任务</option>
            <option value="read_text">有界阅读提供的文本</option>
          </select></label
        ><label
          >触发任务名称<input
            v-model="form.title"
            required
            maxlength="200" /></label
        ><label
          >{{ form.action === "read_text" ? "授权阅读的文本" : "提醒或任务内容"
          }}<textarea
            v-model="form.content"
            rows="5"
            required
            :maxlength="form.action === 'read_text' ? 64000 : 16000"
          ></textarea></label
        ><button class="primary" :disabled="busy">保存停用草稿</button>
      </form>
    </details>
    <article v-for="item in items" :key="item.id" class="job-card">
      <div class="section-heading">
        <strong>{{ item.action.title }}</strong
        ><span class="badge"
          >{{ item.enabled ? "已启用" : "已停用" }} · v{{ item.revision }}</span
        >
      </div>
      <p class="muted">
        {{
          item.rule.type === "event"
            ? "等待新事件"
            : item.next_at
              ? new Date(item.next_at * 1000).toLocaleString("zh-CN", {
                  timeZone: "Asia/Shanghai",
                })
              : "启用前未排程"
        }}
      </p>
      <div class="form-actions">
        <button class="secondary" :disabled="busy" @click="toggle(item)">
          {{ item.enabled ? "停用" : "预演并启用" }}</button
        ><button
          class="text-button danger"
          :disabled="busy"
          @click="remove(item)"
        >
          删除规则
        </button>
      </div>
    </article>
    <details v-if="results.length">
      <summary>执行结果（{{ results.length }}）</summary>
      <article v-for="result in results" :key="result.id" class="job-card">
        <span class="badge">{{
          {
            queued: "处理中",
            completed: "已完成",
            failed: "失败",
            cancelled: "已取消",
            interrupted: "已中断",
          }[result.status as string] || result.status
        }}</span>
        <p class="record-body">{{ result.result || "结果尚未确认" }}</p>
      </article>
    </details>
  </section>
</template>
