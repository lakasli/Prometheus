<script setup lang="ts">
import { onMounted, reactive, ref } from "vue";
import { api } from "../api";
const emit = defineEmits<{ error: [unknown] }>(),
  items = ref<any[]>([]),
  busy = ref(false),
  notice = ref(""),
  form = reactive({
    name: "",
    description: "",
    engine: "wasm_component",
    source: "",
  });
async function task(action: () => Promise<void>) {
  if (busy.value) return;
  busy.value = true;
  try {
    await action();
  } catch (error) {
    emit("error", error);
  } finally {
    busy.value = false;
  }
}
async function load() {
  items.value = (await api<any>("/plugins")).items;
}
async function create() {
  await task(async () => {
    await api("/plugins", "POST", {
      manifest: {
        name: form.name,
        description: form.description,
        engine: form.engine,
        permissions: [],
        abi: "json.v1",
      },
      source: form.source,
    });
    form.source = "";
    await load();
    notice.value = "制品草稿已保存。测试、摘要批准和激活各自保留门禁。";
  });
}
async function inspect(item: any) {
  await task(async () => {
    const result = await api<any>(
      `/plugins/${item.id}/test`,
      "POST",
      {},
      item.revision,
    );
    notice.value = result.reason;
    await load();
  });
}
onMounted(() => task(load));
</script>
<template>
  <section class="panel feature-section">
    <div class="section-heading">
      <div>
        <h2>插件制品与安全检查</h2>
        <p>
          版本、源码摘要与测试状态分别保存；当前Windows隔离证明不足，执行保持禁用。
        </p>
      </div>
      <span class="badge">执行禁用</span>
    </div>
    <p v-if="notice" class="info-note" role="status">{{ notice }}</p>
    <details>
      <summary>登记空权限制品草稿</summary>
      <form @submit.prevent="create">
        <label
          >插件名称<input v-model="form.name" required maxlength="128" /></label
        ><label
          >用途说明<textarea
            v-model="form.description"
            required
            maxlength="4000"
          ></textarea></label
        ><label
          >制品类型<select v-model="form.engine">
            <option value="wasm_component">Rust / WIT 组件</option>
            <option value="python_action">受管 Python 动作</option>
          </select></label
        ><label
          >待审源码<textarea
            v-model="form.source"
            rows="8"
            required
            maxlength="64000"
            spellcheck="false"
          ></textarea></label
        ><button class="primary" :disabled="busy">保存插件草稿</button>
      </form>
    </details>
    <article v-for="item in items" :key="item.id" class="job-card">
      <h3>{{ item.manifest.name }}</h3>
      <p>{{ item.manifest.description }}</p>
      <p class="muted">
        版本 {{ item.revision }} ·
        {{ item.status === "test_blocked" ? "测试被门禁阻止" : "草稿" }} ·
        空权限
      </p>
      <p class="digest">SHA256 {{ item.digest }}</p>
      <p v-if="item.test" class="info-note">{{ item.test.reason }}</p>
      <button class="secondary" :disabled="busy" @click="inspect(item)">
        检查当前安全条件
      </button>
    </article>
  </section>
</template>
