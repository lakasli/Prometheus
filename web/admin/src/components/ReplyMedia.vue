<script setup lang="ts">
import type { ReplyItem } from "../types";
const props = defineProps<{ items: ReplyItem[]; conversationId: string }>();
function url(item: ReplyItem) {
  if (!item.asset_id || !/^[a-f0-9-]{36}$/.test(item.asset_id)) return undefined;
  return item.type === "voice"
    ? `/api/admin/v1/speech/assets/${item.asset_id}`
    : `/api/admin/v1/conversations/${encodeURIComponent(props.conversationId)}/media/${item.asset_id}`;
}
</script>
<template>
  <div class="reply-items">
    <div v-for="(item, index) in items" :key="index" class="reply-item">
      <p class="message-content">{{ item.text }}</p>
      <audio v-if="item.type === 'voice' && url(item)" :src="url(item)" controls preload="none" />
      <img v-else-if="item.type === 'image' && url(item)" :src="url(item)" :alt="item.text" loading="lazy" />
      <video v-else-if="item.type === 'video' && url(item)" :src="url(item)" controls preload="none" />
      <small v-if="index > 0" class="muted">渠道发送间隔至少 {{ (item.delay_ms / 1000).toFixed(1) }} 秒</small>
    </div>
  </div>
</template>
<style scoped>
.reply-items { display: grid; gap: 1rem; }
.reply-item img, .reply-item video { display: block; max-width: 100%; max-height: 480px; border-radius: 12px; }
.reply-item audio { display: block; max-width: 100%; }
.reply-item p { margin: 0 0 .4rem; }
.muted { opacity: .65; font-size: .75rem; }
</style>
