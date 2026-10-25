<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api, errorMessage } from "../api";
interface Item { id: string; status: string; error: string | null; unsent_parts: number }
const items = ref<Item[]>([]), busy = ref(false), error = ref("");
const checked = ref<Record<string, boolean>>({});
async function load() { busy.value=true; try { items.value=(await api<{items:Item[]}>("/qq/deliveries")).items; } catch(e) { error.value=errorMessage(e); } finally { busy.value=false; } }
async function resolve(item: Item) { if(!checked.value[item.id])return;busy.value=true;error.value="";try { await api(`/qq/deliveries/${item.id}/resolve`,{method:"POST",body:{expected_status:"unknown",abandon_remaining:true,checked_client:true}});await load(); } catch(e) { error.value=errorMessage(e); } finally { busy.value=false; } }
onMounted(load);
</script>
<template>
  <details>
    <summary>QQ 不确定投递核对（{{ items.filter(i=>i.status==='unknown').length }}）</summary>
    <p>网络中断时平台可能已经收到了消息。先在 QQ 中核对；旧消息不会自动重发。放弃未发部分后，当前会话才会继续发送新的回复。</p>
    <p v-if="error" role="alert">{{ error }}</p>
    <button class="button secondary small" :disabled="busy" @click="load">刷新回执</button>
    <p v-if="!items.length" class="muted">当前账号没有待核对回执。</p>
    <div v-for="item in items" :key="item.id" class="receipt">
      <p>{{ item.status==='unknown'?'待核对':'已放弃未发部分，原回执仍未知' }} · {{ item.error }} · 剩余 {{ item.unsent_parts }} 条</p>
      <template v-if="item.status==='unknown'"><label><input v-model="checked[item.id]" type="checkbox" />我已核对 QQ，放弃这一轮剩余内容</label><button class="button secondary small" :disabled="busy || !checked[item.id]" @click="resolve(item)">放弃未发部分并恢复后续对话</button></template>
    </div>
  </details>
</template>
<style scoped>.receipt {padding:1rem 0;border-bottom:1px solid #ddd}.receipt label{display:flex;gap:.5rem;align-items:center;margin-bottom:.5rem}.receipt input{width:auto}</style>
