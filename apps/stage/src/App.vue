<script setup lang="ts">
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  shallowRef,
  watch,
} from "vue";
import { ensureCubismCore } from "./cubism-core";
import type { StageMetrics, StageRenderer } from "./cubism-stage";
import { expressionGroups } from "./presets";
import { useStageConversation } from "./stage-conversation";

type ExpressionGroup = (typeof expressionGroups)[number];

const presets = expressionGroups.flatMap((group) => group.presets);
const defaultIds = expressionGroups.flatMap((group) =>
  group.defaultId ? [group.defaultId] : [],
);
const expressionPaths = Object.fromEntries(
  presets.map(({ id, path }) => [id, path]),
);
const faceIds = new Set(
  expressionGroups.find(({ id }) => id === "face")?.presets.map(({ id }) => id),
);
const minZoom = 60;
const maxZoom = 180;
const baseZoom = 100;

const stage = ref<HTMLElement | null>(null);
const canvas = ref<HTMLCanvasElement | null>(null);
const state = ref<"loading" | "ready" | "error">("loading");
const metrics = shallowRef<StageMetrics | null>(null);
const error = ref("");
const fullscreenError = ref("");
const zoom = ref(baseZoom);
const activePresets = shallowRef(new Set(defaultIds));
const usingDefaults = computed(
  () =>
    activePresets.value.size === defaultIds.length &&
    defaultIds.every((id) => activePresets.value.has(id)),
);
const fullscreen = ref(false);
const fullscreenAvailable = ref(false);
const panel = ref<"model" | "chat">("model");
const {
  status: chatStatus,
  conversations,
  selected,
  selectedChannel,
  draft,
  busy: chatBusy,
  error: chatError,
  feedError,
  bubbles,
  expression: chatExpression,
  qqLink,
  qqChallenge,
  canSend,
  refresh: refreshChat,
  selectConversation,
  createConversation,
  requestQqLink,
  revokeQqLink,
  sendMessage,
} = useStageConversation(faceIds);
const qqDeadline = computed(() =>
  qqChallenge.value
    ? new Date(qqChallenge.value.expiresAt).toLocaleTimeString("zh-CN", {
        hour12: false,
      })
    : undefined,
);
const channelLabels = {
  native: "本机",
  qq: "QQ 私聊",
  weixin: "微信私聊",
} as const;
const conversationGroups = computed(() =>
  (Object.keys(channelLabels) as (keyof typeof channelLabels)[]).flatMap(
    (channel) => {
      const items = conversations.value.filter(
        (item) => item.channel === channel,
      );
      if (!items.length) return [];
      return [
        {
          channel,
          label: channelLabels[channel],
          items: items.map(({ id, title }, index) => ({
            id,
            label:
              channel === "native"
                ? title
                : items.length === 1
                  ? channelLabels[channel]
                  : `${channelLabels[channel]} ${index + 1}`,
          })),
        },
      ];
    },
  ),
);

let renderer: StageRenderer | undefined;
let controller: AbortController | undefined;

async function loadStage() {
  const target = canvas.value;
  if (!target) return;
  const startedAt = performance.now();
  controller?.abort();
  renderer?.dispose();
  renderer = undefined;
  const load = new AbortController();
  controller = load;
  state.value = "loading";
  error.value = "";
  metrics.value = null;
  zoom.value = baseZoom;
  activePresets.value = new Set(defaultIds);

  try {
    await ensureCubismCore("/stage/core/live2dcubismcore.min.js", load.signal);
    // Some Framework modules read Core constants as soon as they are imported.
    const { createCubismStage } = await import("./cubism-stage");
    const next = await createCubismStage(target, {
      modelUrl: `/stage/model/${encodeURIComponent("大肥鱼.model3.json")}`,
      startedAt,
      idlePath: "Animation/IDLE.motion3.json",
      expressions: expressionPaths,
      signal: load.signal,
      onMetrics(value) {
        if (!load.signal.aborted) metrics.value = value;
      },
      onFatalError(message) {
        if (load.signal.aborted) return;
        error.value = message;
        state.value = "error";
      },
    });
    if (load.signal.aborted) {
      next.dispose();
      return;
    }
    renderer = next;
    applyPresets();
    if (state.value === "loading") state.value = "ready";
  } catch (cause) {
    if (load.signal.aborted) return;
    error.value = cause instanceof Error ? cause.message : String(cause);
    state.value = "error";
  }
}

function setZoom(percent: number) {
  const value = Math.min(maxZoom, Math.max(minZoom, percent));
  renderer?.setZoom(value / baseZoom);
  zoom.value = value;
}

function updateZoom(event: Event) {
  setZoom((event.target as HTMLInputElement).valueAsNumber);
}

function setPresets(selection: Set<string>) {
  activePresets.value = selection;
  applyPresets();
}

function applyPresets() {
  const overlay = chatExpression.value;
  renderer?.setExpressions(
    presets
      .filter(({ id }) =>
        overlay !== undefined && faceIds.has(id)
          ? id === overlay
          : activePresets.value.has(id),
      )
      .map(({ id }) => id),
  );
}

watch(chatExpression, applyPresets);

function selectPreset(group: ExpressionGroup, id?: string) {
  const selection = new Set(activePresets.value);
  if (group.selection === "single") {
    for (const preset of group.presets) selection.delete(preset.id);
    if (id !== undefined) selection.add(id);
  } else if (id !== undefined && !selection.delete(id)) {
    selection.add(id);
  }
  setPresets(selection);
}

function resetPresets() {
  setPresets(new Set(defaultIds));
}

function changeConversation(event: Event) {
  selectConversation((event.target as HTMLSelectElement).value);
}

function syncFullscreen() {
  fullscreen.value = document.fullscreenElement === stage.value;
}

async function toggleFullscreen() {
  if (!stage.value) return;
  fullscreenError.value = "";
  try {
    if (fullscreen.value) await document.exitFullscreen();
    else await stage.value.requestFullscreen();
  } catch {
    fullscreenError.value = "浏览器未能切换全屏。";
  }
}

onMounted(() => {
  fullscreenAvailable.value = document.fullscreenEnabled;
  document.addEventListener("fullscreenchange", syncFullscreen);
  void loadStage();
});

onBeforeUnmount(() => {
  document.removeEventListener("fullscreenchange", syncFullscreen);
  controller?.abort();
  renderer?.dispose();
});
</script>

<template>
  <main ref="stage" class="stage-shell">
    <div class="stage-layout">
      <header class="masthead">
        <div class="brand">
          <svg aria-hidden="true" viewBox="0 0 24 24" width="24" height="24">
            <path
              d="M12 1.5 14.8 9.2 22.5 12l-7.7 2.8-2.8 7.7-2.8-7.7L1.5 12l7.7-2.8L12 1.5Z"
              fill="currentColor"
            />
          </svg>
          <span>Prometheus</span>
        </div>
        <span class="local-label"><i></i> 本机舞台</span>
      </header>

      <div class="stage-grid">
        <section
          class="viewport"
          aria-label="大肥鱼 Live2D 展示区"
          :aria-busy="state === 'loading'"
          :data-stage-state="state"
          :data-stage-expression="chatExpression"
          :data-stage-fps="metrics?.fps"
          :data-stage-frames="metrics?.frames"
          :data-stage-first-frame-ms="metrics?.firstFrameMs"
          :data-stage-max-texture-size="metrics?.maxTextureSize"
          :data-stage-textures="
            metrics?.textures
              .map(({ width, height }) => `${width}×${height}`)
              .join(',')
          "
        >
          <canvas ref="canvas" aria-label="大肥鱼 Live2D 模型"></canvas>
          <div
            v-if="state === 'ready' && selected"
            :key="selected"
            class="stage-bubbles"
            role="log"
            aria-label="对话气泡"
            aria-relevant="additions"
          >
            <TransitionGroup
              v-for="role in ['user', 'assistant']"
              :key="role"
              tag="div"
              name="stage-bubble"
              class="bubble-column"
              :data-role="role"
            >
              <article
                v-for="bubble in bubbles.filter((item) => item.role === role)"
                :key="bubble.id"
                class="bubble"
                :data-stage-message-id="bubble.id"
                :data-stage-message-role="bubble.role"
              >
                <span class="bubble-speaker">{{
                  role === "assistant"
                    ? "大肥鱼"
                    : selectedChannel === "native"
                      ? "你"
                      : "对方"
                }}</span>
                <p tabindex="0">{{ bubble.text }}</p>
              </article>
            </TransitionGroup>
          </div>
          <div class="viewport-top">
            <span class="take"><i :class="state"></i> LIVE2D</span>
            <button
              class="fullscreen"
              type="button"
              :aria-pressed="fullscreen"
              :disabled="!fullscreenAvailable"
              @click="toggleFullscreen"
            >
              <svg
                aria-hidden="true"
                viewBox="0 0 20 20"
                width="16"
                height="16"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
                stroke-linejoin="round"
              >
                <path
                  v-if="fullscreen"
                  d="M3 7h4V3m10 4h-4V3M3 13h4v4m10-4h-4v4"
                />
                <path v-else d="M7 3H3v4m10-4h4v4M7 17H3v-4m10 4h4v-4" />
              </svg>
              {{ fullscreen ? "退出全屏" : "全屏" }}
            </button>
          </div>

          <div v-if="state === 'loading'" class="stage-message" role="status">
            <span class="loader" aria-hidden="true"></span>
            <strong>正在加载大肥鱼</strong>
            <span>读取原尺寸贴图</span>
          </div>
          <div v-else-if="state === 'error'" class="stage-message stage-error">
            <div role="alert">
              <strong>舞台暂时无法显示</strong>
              <p>{{ error }}</p>
            </div>
            <button type="button" @click="loadStage">重新加载</button>
          </div>

          <footer v-if="state === 'ready'" class="viewport-bottom">
            <span>原尺寸贴图</span>
            <span>{{
              metrics && metrics.fps > 0
                ? `${metrics.fps.toFixed(1)} FPS`
                : "帧率采样中"
            }}</span>
          </footer>
          <div v-if="fullscreenError" class="fullscreen-error" role="status">
            {{ fullscreenError }}
          </div>
        </section>

        <aside class="side-panel" aria-label="舞台控制与对话">
          <div class="model-heading">
            <p class="eyebrow">MODEL / 01</p>
            <h1>大肥鱼</h1>
            <p class="model-name">DeepSeek-chan</p>
          </div>

          <div class="panel-switch" role="group" aria-label="侧栏内容">
            <button
              type="button"
              :aria-pressed="panel === 'model'"
              aria-controls="stage-model-panel"
              @click="panel = 'model'"
            >
              模型
            </button>
            <button
              type="button"
              :aria-pressed="panel === 'chat'"
              aria-controls="stage-chat-panel"
              @click="panel = 'chat'"
            >
              对话
            </button>
          </div>

          <section
            v-show="panel === 'chat'"
            id="stage-chat-panel"
            class="chat-panel"
            aria-label="舞台对话"
          >
            <p v-if="chatStatus === 'connecting'" role="status">
              正在连接对话服务
            </p>
            <template v-else-if="chatStatus !== 'signed-in'">
              <p>
                {{
                  chatStatus === "signed-out"
                    ? "登录后可选择本机对话。"
                    : "对话连接暂时不可用。"
                }}
              </p>
              <div class="chat-actions">
                <a
                  v-if="chatStatus === 'signed-out'"
                  href="/admin/"
                  target="_blank"
                  rel="noopener"
                  >前往管理页</a
                >
                <button class="reset" type="button" @click="refreshChat">
                  重新连接
                </button>
              </div>
            </template>
            <form v-else class="chat-form" @submit.prevent="sendMessage">
              <label for="stage-conversation">当前对话</label>
              <select
                id="stage-conversation"
                :value="selected"
                :disabled="chatBusy"
                @change="changeConversation"
              >
                <option value="">选择要展示的对话</option>
                <optgroup
                  v-for="group in conversationGroups"
                  :key="group.channel"
                  :label="group.label"
                >
                  <option
                    v-for="item in group.items"
                    :key="item.id"
                    :value="item.id"
                  >
                    {{ item.label }}
                  </option>
                </optgroup>
              </select>
              <div class="chat-actions">
                <button
                  class="reset"
                  type="button"
                  :disabled="chatBusy"
                  @click="createConversation"
                >
                  新建对话
                </button>
                <button
                  class="reset"
                  type="button"
                  :disabled="chatBusy"
                  @click="refreshChat"
                >
                  刷新列表
                </button>
              </div>
              <template v-if="selectedChannel === 'native'">
                <label for="stage-message-input">消息</label>
                <textarea
                  id="stage-message-input"
                  v-model="draft"
                  rows="3"
                  maxlength="16000"
                  :disabled="chatBusy"
                ></textarea>
                <button class="chat-send" type="submit" :disabled="!canSend">
                  发送消息
                </button>
              </template>
              <p
                v-else-if="
                  selectedChannel === 'qq' || selectedChannel === 'weixin'
                "
              >
                在{{
                  selectedChannel === "qq" ? " QQ " : "微信"
                }}中继续聊天，消息会显示在舞台上。
              </p>
              <p v-if="chatError || feedError" class="chat-error" role="status">
                {{ chatError || feedError }}
              </p>
              <section
                v-if="qqLink"
                class="qq-link"
                aria-labelledby="qq-link-title"
              >
                <h2 id="qq-link-title">QQ 私聊绑定</h2>
                <p v-if="qqLink.linked" role="status">QQ 已绑定。</p>
                <p v-else-if="qqLink.pending" role="status">等待 QQ 验证。</p>
                <p v-else-if="!qqLink.configured">
                  在管理页配置 QQ 渠道后即可绑定。
                </p>
                <div v-if="qqChallenge" class="qq-challenge">
                  <p>请在你与大肥鱼的 QQ 私聊中发送这条命令。</p>
                  <code>{{ qqChallenge.command }}</code>
                  <p>有效至 {{ qqDeadline }}</p>
                </div>
                <div
                  v-if="qqLink.configured || qqLink.linked || qqLink.pending"
                  class="chat-actions"
                >
                  <button
                    v-if="qqLink.configured && !qqLink.linked"
                    class="reset"
                    type="button"
                    :disabled="chatBusy"
                    @click="requestQqLink"
                  >
                    {{
                      qqLink.pending ? "重新生成绑定命令" : "生成 QQ 绑定命令"
                    }}
                  </button>
                  <button
                    v-if="qqLink.linked || qqLink.pending"
                    class="reset"
                    type="button"
                    :disabled="chatBusy"
                    @click="revokeQqLink"
                  >
                    {{ qqLink.linked ? "解除 QQ 绑定" : "取消绑定请求" }}
                  </button>
                </div>
              </section>
            </form>
          </section>

          <div
            v-show="panel === 'model'"
            id="stage-model-panel"
            class="controls"
          >
            <fieldset :disabled="state !== 'ready'">
              <legend>画面缩放</legend>
              <div class="zoom-value">
                <output for="stage-zoom">{{ zoom }}%</output>
                <button
                  type="button"
                  class="reset"
                  :disabled="zoom === baseZoom"
                  @click="setZoom(baseZoom)"
                >
                  重置
                </button>
              </div>
              <div class="zoom-row">
                <button
                  type="button"
                  aria-label="缩小模型"
                  :disabled="zoom <= minZoom"
                  @click="setZoom(zoom - 10)"
                >
                  −
                </button>
                <input
                  id="stage-zoom"
                  type="range"
                  aria-label="模型缩放"
                  :min="minZoom"
                  :max="maxZoom"
                  :value="zoom"
                  @input="updateZoom"
                />
                <button
                  type="button"
                  aria-label="放大模型"
                  :disabled="zoom >= maxZoom"
                  @click="setZoom(zoom + 10)"
                >
                  +
                </button>
              </div>
            </fieldset>

            <section class="preset-panel" aria-labelledby="stage-preset-title">
              <div class="preset-heading">
                <h2 id="stage-preset-title">模型配置</h2>
                <button
                  class="reset"
                  type="button"
                  :disabled="state !== 'ready' || usingDefaults"
                  @click="resetPresets"
                >
                  全部恢复默认
                </button>
              </div>
              <div
                class="preset-scroll"
                role="region"
                aria-label="模型配置分组"
                tabindex="0"
              >
                <fieldset
                  v-for="group in expressionGroups"
                  :key="group.id"
                  class="preset-group"
                  :data-preset-group="group.id"
                  :disabled="state !== 'ready'"
                >
                  <legend>{{ group.label }}</legend>
                  <div class="expressions">
                    <button
                      v-if="group.resetLabel"
                      type="button"
                      :aria-pressed="
                        !group.presets.some(({ id }) => activePresets.has(id))
                      "
                      @click="selectPreset(group)"
                    >
                      {{ group.resetLabel }}
                    </button>
                    <button
                      v-for="preset in group.presets"
                      :key="preset.id"
                      type="button"
                      :data-preset-id="preset.id"
                      :aria-pressed="activePresets.has(preset.id)"
                      @click="selectPreset(group, preset.id)"
                    >
                      {{ preset.label }}
                    </button>
                  </div>
                </fieldset>
              </div>
            </section>
          </div>

          <footer class="credits">
            <p>神宫凉子免费配布模型</p>
            <dl>
              <div>
                <dt>原画及拆分</dt>
                <dd>神宫凉子</dd>
              </div>
              <div>
                <dt>Live2D 建模</dt>
                <dd>霓虹猫_NeonNeko</dd>
              </div>
            </dl>
          </footer>
        </aside>
      </div>
    </div>
  </main>
</template>
