<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, shallowRef } from "vue";
import { ensureCubismCore } from "./cubism-core";
import type { StageMetrics, StageRenderer } from "./cubism-stage";

const expressions = [
  { id: "blush", label: "脸红", path: "Exp3/脸红.exp3.json" },
  { id: "love", label: "爱心眼", path: "Exp3/爱心眼.exp3.json" },
  { id: "dizzy", label: "圈圈眼", path: "Exp3/圈圈眼.exp3.json" },
  { id: "tears", label: "流泪", path: "Exp3/流泪.exp3.json" },
] as const;
const expressionPaths = Object.fromEntries(
  expressions.map(({ id, path }) => [id, path]),
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
const activeExpression = ref<(typeof expressions)[number]["id"] | null>(null);
const fullscreen = ref(false);
const fullscreenAvailable = ref(false);

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
  activeExpression.value = null;

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

function setExpression(id: typeof activeExpression.value) {
  if (!renderer) return;
  renderer.setExpression(id);
  activeExpression.value = id;
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
          <div class="stage-light" aria-hidden="true"></div>
          <canvas ref="canvas" aria-label="大肥鱼 Live2D 模型"></canvas>
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

        <aside class="side-panel" aria-label="舞台控制">
          <div class="model-heading">
            <p class="eyebrow">MODEL / 01</p>
            <h1>大肥鱼</h1>
            <p class="model-name">DeepSeek-chan</p>
          </div>

          <div class="controls">
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

            <fieldset :disabled="state !== 'ready'">
              <legend>表情</legend>
              <div class="expressions">
                <button
                  type="button"
                  :aria-pressed="activeExpression === null"
                  @click="setExpression(null)"
                >
                  默认
                </button>
                <button
                  v-for="expression in expressions"
                  :key="expression.id"
                  type="button"
                  :aria-pressed="activeExpression === expression.id"
                  @click="setExpression(expression.id)"
                >
                  {{ expression.label }}
                </button>
              </div>
            </fieldset>
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
