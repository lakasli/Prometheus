<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { api } from "../api";
import { scheduleStatus } from "../schedule-labels";
const emit = defineEmits<{ error: [unknown]; changed: [] }>(),
  items = ref<any[]>([]),
  total = ref(0),
  offset = ref(0),
  busy = ref(false),
  notice = ref("");
let disposed = false,
  controller: AbortController | undefined;
async function load() {
  controller?.abort();
  const current = new AbortController();
  controller = current;
  const result = await api<any>(
    `/schedule/text-activities?limit=20&offset=${offset.value}`,
    "GET",
    undefined,
    undefined,
    current.signal,
  );
  if (!disposed && !current.signal.aborted) {
    items.value = result.items;
    total.value = result.total;
  }
}
async function act(fn: () => Promise<void>) {
  if (busy.value || disposed) return;
  busy.value = true;
  try {
    await fn();
  } catch (e) {
    if (!disposed && !(e instanceof Error && e.name === "AbortError"))
      emit("error", e);
  } finally {
    if (!disposed) busy.value = false;
  }
}
function date(at: number | null, zone: string) {
  return at === null
    ? "尚未发生"
    : new Intl.DateTimeFormat("zh-CN", {
        timeZone: zone,
        dateStyle: "medium",
        timeStyle: "medium",
        hour12: false,
      }).format(at * 1000);
}
async function forget(item: any) {
  if (
    !confirm(
      "遗忘这份文字成果、缓存副本和私有完成索引？尚未执行的安排会停止；真实保存时间、原聊天与离线备份保留。",
    )
  )
    return;
  await act(async () => {
    await api(
      `/schedule/text-activities/${item.activity_id}/forget`,
      "POST",
      {},
      item.revision,
    );
    notice.value = "文字成果及派生副本已清除，实际历史仍如实保留。";
    await load();
    emit("changed");
  });
}
onMounted(() => act(load));
onUnmounted(() => {
  disposed = true;
  controller?.abort();
});
</script>
<template>
  <section class="panel daily-text-panel" aria-label="日常文字成果">
    <h2>日常文字成果</h2>
    <p>
      这里记录聊天中按明确授权登记的文字安排。已准备不等于已完成；只在授权时段实际保存后记录完成。成果保留在原来源，本页不会把它发送给其他人。
    </p>
    <button :disabled="busy" @click="act(load)">刷新文字成果</button>
    <p v-if="notice" role="status">{{ notice }}</p>
    <p v-if="!items.length">尚无已登记的文字安排。</p>
    <article v-for="item in items" :key="item.activity_id" class="text-result">
      <h3>{{ item.title ?? "文字成果已失效" }}</h3>
      <p>
        {{
          item.state === "prepared"
            ? "已准备，等待实际保存"
            : item.state === "saved"
              ? "已实际保存"
              : "成果内容已清除"
        }}
        · 日程{{ scheduleStatus(item.activity_state) }}
      </p>
      <p>
        授权时段：{{ date(item.start_at, item.time_zone) }}—{{
          date(item.end_at, item.time_zone)
        }}
      </p>
      <p>
        准备时间：{{ date(item.prepared_at, item.time_zone) }} · 实际保存：{{
          date(item.saved_at, item.time_zone)
        }}
      </p>
      <p class="muted">
        {{ item.source_owner_id }} · {{ item.time_zone }} · 不涉及渠道投递
      </p>
      <details v-if="item.content !== null">
        <summary>
          {{
            item.state === "saved" ? "查看已保存的完整成果" : "查看已准备的正文"
          }}
        </summary>
        <pre>{{ item.content }}</pre>
        <small>{{ item.characters }} 个字</small>
      </details>
      <p v-else>来源、授权、活动或人工决定已使内容失效；不会从旧缓存恢复。</p>
      <button
        v-if="item.can_manage && item.state !== 'invalidated'"
        class="danger"
        :disabled="busy"
        @click="forget(item)"
      >
        遗忘文字成果
      </button>
    </article>
    <div class="text-pages">
      <button
        :disabled="busy || offset === 0"
        @click="
          act(async () => {
            offset -= 20;
            await load();
          })
        "
      >
        上一页文字成果</button
      ><span>共{{ total }}项</span
      ><button
        :disabled="busy || offset + 20 >= total"
        @click="
          act(async () => {
            offset += 20;
            await load();
          })
        "
      >
        下一页文字成果
      </button>
    </div>
  </section>
</template>
<style scoped>
.daily-text-panel {
  margin-top: 24px;
  min-width: 0;
}
.daily-text-panel p {
  overflow-wrap: anywhere;
}
.text-result {
  border: 1px solid var(--border, #d9dce2);
  border-radius: 10px;
  padding: 16px;
  margin: 14px 0;
  min-width: 0;
}
.text-result pre {
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  max-height: 28rem;
  overflow: auto;
}
.text-pages {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}
</style>
