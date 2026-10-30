<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>(),
  policy = ref<any>(null),
  status = ref<any>(null),
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
      原唤醒期限：{{
        new Date(status.state.wake_at * 1000).toLocaleString("zh-CN", {
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
  </section>
</template>
