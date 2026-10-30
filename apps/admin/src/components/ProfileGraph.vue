<script setup lang="ts">
import {
  computed,
  onMounted,
  onUnmounted,
  reactive,
  ref,
  shallowRef,
  useId,
  watch,
} from "vue";
import { layoutRelationshipGraph, graphEdgePath } from "../relationship-graph";
import type { RelationshipGraphNode } from "../relationship-graph";
import {
  createGraphMotion,
  stepGraphMotion,
} from "../relationship-graph-motion";
import type { GraphMotionState } from "../relationship-graph-motion";
interface GraphPerson {
  id: string;
  data: { name: string };
  merged_into?: string | null;
}
interface InputEdge {
  id: string;
  from_id: string;
  to_id: string;
  relationship_type: string;
  status?: string;
}
interface MotionEdge {
  id: string;
  from_person_id: string;
  to_person_id: string;
  relationship_type: string;
}
const props = defineProps<{
  items: GraphPerson[];
  edges: InputEdge[];
  selectedId?: string | null;
  busy?: boolean;
}>();
const emit = defineEmits<{ select: [string] }>();
const uid = useId(),
  opened = ref(true),
  busy = computed(() => props.busy ?? false);
let disposed = false;
const graphPeople = computed(() =>
  props.items.filter((p) => !p.merged_into).slice(0, 50),
);
const graphEdges = computed(() => {
  const ids = new Set(graphPeople.value.map((p) => p.id));
  return props.edges
    .filter(
      (e) =>
        (!e.status || e.status === "approved") &&
        ids.has(e.from_id) &&
        ids.has(e.to_id),
    )
    .map((e) => ({ ...e, from_person_id: e.from_id, to_person_id: e.to_id }));
});
const targetPositions = computed(() =>
  layoutRelationshipGraph(graphPeople.value, graphEdges.value),
);
function choose(p: GraphPerson) {
  emit("select", p.id);
}
function name(id: string) {
  return (
    graphPeople.value.find((p) => p.id === id)?.data.name ?? "当前页之外的人物"
  );
}
function label(p: GraphPerson) {
  const text = [...p.data.name];
  return text.length > 8 ? text.slice(0, 7).join("") + "…" : p.data.name;
}
const positions = shallowRef(new Map<string, RelationshipGraphNode>());
const selectedId = computed(() => props.selectedId ?? null);
const hoveredId = ref<string | null>(null);
const focusedId = ref<string | null>(null);
const draggedId = ref<string | null>(null);
const highlightedId = computed(
  () =>
    draggedId.value || hoveredId.value || focusedId.value || selectedId.value,
);
const detailLoading = ref(false);
const zoom = ref(1);
const renderedZoom = ref(1);
const pan = reactive({ x: 0, y: 0 });
const graphSvg = ref<SVGSVGElement | null>(null);
const graphViewport = ref<SVGGElement | null>(null);
const dragging = ref(false);
const animating = ref(false);
const reducedMotion = ref(
  window.matchMedia("(prefers-reduced-motion: reduce)").matches,
);
let motionMedia: MediaQueryList | undefined;
let motion: GraphMotionState | null = null;
let animationFrame: number | null = null;
let lastFrame = 0;
let resumeMotion = false;
let clickSuppression: { id: string; until: number } | null = null;
let nodeDrag: {
  pointer: number;
  id: string;
  startX: number;
  startY: number;
  offsetX: number;
  offsetY: number;
  moved: boolean;
} | null = null;
let pinned: { id: string; x: number; y: number } | null = null;
let pointerCapture: SVGElement | null = null;
let dragStart: {
  pointer: number;
  x: number;
  y: number;
  panX: number;
  panY: number;
} | null = null;
const connectedIds = computed(() => {
  const ids = new Set<string>();
  if (highlightedId.value) ids.add(highlightedId.value);
  for (const edge of graphEdges.value) {
    if (edge.from_person_id === highlightedId.value) ids.add(edge.to_person_id);
    if (edge.to_person_id === highlightedId.value) ids.add(edge.from_person_id);
  }
  return ids;
});
function degree(id: string) {
  return graphEdges.value.filter(
    (edge) => edge.from_person_id === id || edge.to_person_id === id,
  ).length;
}
function edgePath(edge: MotionEdge) {
  return graphEdgePath(
    edge.id,
    positions.value.get(edge.from_person_id)!,
    positions.value.get(edge.to_person_id)!,
    graphEdges.value.some(
      (other) =>
        other.from_person_id === edge.to_person_id &&
        other.to_person_id === edge.from_person_id,
    ),
  );
}
function isConnected(edge: MotionEdge) {
  return (
    edge.from_person_id === highlightedId.value ||
    edge.to_person_id === highlightedId.value
  );
}
function stopMotion() {
  if (animationFrame !== null) window.cancelAnimationFrame(animationFrame);
  animationFrame = null;
  animating.value = false;
  lastFrame = 0;
}
function settleMotion() {
  stopMotion();
  renderedZoom.value = zoom.value;
  if (!motion) return;
  for (let i = 0; i < 182; i++) {
    const step = stepGraphMotion(motion, pinned);
    positions.value = step.positions;
    if (step.settled) break;
  }
}
function wakeMotion() {
  if (
    disposed ||
    !opened.value ||
    !motion ||
    reducedMotion.value ||
    document.hidden
  )
    return;
  if (animationFrame !== null) return;
  animating.value = true;
  lastFrame = 0;
  const frame = (time: number) => {
    animationFrame = null;
    if (
      disposed ||
      !opened.value ||
      !motion ||
      reducedMotion.value ||
      document.hidden
    ) {
      stopMotion();
      return;
    }
    const elapsed = lastFrame
      ? Math.min(32, Math.max(8, time - lastFrame))
      : 1000 / 60;
    lastFrame = time;
    const difference = zoom.value - renderedZoom.value;
    renderedZoom.value =
      Math.abs(difference) < 0.001
        ? zoom.value
        : renderedZoom.value +
          difference * (1 - 0.72 ** (elapsed / (1000 / 60)));
    const result = stepGraphMotion(motion, pinned, elapsed);
    positions.value = result.positions;
    // Idle graphs never retain a frame loop, even under a degenerate layout.
    if (result.settled && renderedZoom.value === zoom.value) {
      renderedZoom.value = zoom.value;
      stopMotion();
      return;
    }
    animationFrame = window.requestAnimationFrame(frame);
  };
  animationFrame = window.requestAnimationFrame(frame);
}
function rebuildMotion(entrance = false) {
  stopMotion();
  endPan();
  const target = targetPositions.value;
  const previous =
    entrance && !positions.value.size
      ? new Map(
          [...target].map(([id, node]) => [
            id,
            {
              ...node,
              x: 450 + (node.x - 450) * 0.78,
              y: 340 + (node.y - 340) * 0.78,
            },
          ]),
        )
      : positions.value;
  motion = createGraphMotion(target, graphEdges.value, previous);
  positions.value = stepGraphMotion(motion).positions;
  if (!opened.value || reducedMotion.value || document.hidden) settleMotion();
  else wakeMotion();
}
watch(
  targetPositions,
  () => {
    hoveredId.value = null;
    focusedId.value = null;
    rebuildMotion(true);
  },
  { flush: "sync", immediate: true },
);
function updateMotionPreference() {
  reducedMotion.value = motionMedia?.matches ?? false;
  if (reducedMotion.value) settleMotion();
}
function visibilityChanged() {
  if (document.hidden) {
    resumeMotion = animating.value;
    endPan();
    stopMotion();
    hoveredId.value = null;
  } else if (resumeMotion) {
    resumeMotion = false;
    wakeMotion();
  }
}
function resetGraph() {
  zoom.value = 1;
  pan.x = 0;
  pan.y = 0;
  rebuildMotion();
}
function zoomGraph(amount: number) {
  if (nodeDrag || dragStart) return;
  zoom.value = Math.min(
    2.4,
    Math.max(0.5, Math.round((zoom.value + amount) * 100) / 100),
  );
  if (reducedMotion.value || document.hidden) renderedZoom.value = zoom.value;
  else wakeMotion();
}
function pointInGraph(event: PointerEvent) {
  const matrix = graphViewport.value?.getScreenCTM?.();
  if (matrix && typeof DOMPoint !== "undefined") {
    try {
      const point = new DOMPoint(event.clientX, event.clientY).matrixTransform(
        matrix.inverse(),
      );
      if (Number.isFinite(point.x) && Number.isFinite(point.y)) return point;
    } catch {
      return null;
    }
  }
  const rect = graphSvg.value?.getBoundingClientRect();
  if (!rect || !rect.width || !rect.height) return null;
  const ratio = Math.min(rect.width / 900, rect.height / 680);
  return {
    x:
      ((event.clientX - rect.left - (rect.width - 900 * ratio) / 2) / ratio -
        450 -
        pan.x) /
        renderedZoom.value +
      450,
    y:
      ((event.clientY - rect.top - (rect.height - 680 * ratio) / 2) / ratio -
        340 -
        pan.y) /
        renderedZoom.value +
      340,
  };
}
function startNodeDrag(event: PointerEvent, id: string) {
  if (
    busy.value ||
    event.button !== 0 ||
    event.isPrimary === false ||
    dragStart ||
    nodeDrag
  )
    return;
  zoom.value = renderedZoom.value;
  const point = pointInGraph(event);
  const node = positions.value.get(id);
  if (!point || !node) return;
  nodeDrag = {
    pointer: event.pointerId,
    id,
    startX: event.clientX,
    startY: event.clientY,
    offsetX: node.x - point.x,
    offsetY: node.y - point.y,
    moved: false,
  };
  pointerCapture = event.currentTarget as SVGGElement;
  pointerCapture.setPointerCapture?.(event.pointerId);
}
function activatePerson(person: GraphPerson) {
  if (busy.value) return;
  if (clickSuppression?.id === person.id && Date.now() < clickSuppression.until)
    return;
  void choose(person);
}
function startPan(event: PointerEvent) {
  if (event.button !== 0 || event.isPrimary === false || nodeDrag || dragStart)
    return;
  zoom.value = renderedZoom.value;
  dragStart = {
    pointer: event.pointerId,
    x: event.clientX,
    y: event.clientY,
    panX: pan.x,
    panY: pan.y,
  };
  dragging.value = true;
  pointerCapture = graphSvg.value;
  pointerCapture?.setPointerCapture?.(event.pointerId);
}
function movePan(event: PointerEvent) {
  if (nodeDrag && event.pointerId === nodeDrag.pointer) {
    if (
      !nodeDrag.moved &&
      Math.hypot(
        event.clientX - nodeDrag.startX,
        event.clientY - nodeDrag.startY,
      ) < 4
    )
      return;
    const point = pointInGraph(event);
    const node = positions.value.get(nodeDrag.id);
    if (!point || !node || !motion) return;
    nodeDrag.moved = true;
    draggedId.value = nodeDrag.id;
    pinned = {
      id: nodeDrag.id,
      x: Math.max(
        26 + node.radius,
        Math.min(874 - node.radius, point.x + nodeDrag.offsetX),
      ),
      y: Math.max(
        26 + node.radius,
        Math.min(654 - node.radius, point.y + nodeDrag.offsetY),
      ),
    };
    positions.value = stepGraphMotion(motion, pinned).positions;
    if (!reducedMotion.value) wakeMotion();
    return;
  }
  if (!dragStart || event.pointerId !== dragStart.pointer) return;
  const rect = graphSvg.value?.getBoundingClientRect();
  if (!rect || !rect.width || !rect.height) return;
  const ratio = Math.min(rect.width / 900, rect.height / 680);
  pan.x = Math.max(
    -900,
    Math.min(900, dragStart.panX + (event.clientX - dragStart.x) / ratio),
  );
  pan.y = Math.max(
    -680,
    Math.min(680, dragStart.panY + (event.clientY - dragStart.y) / ratio),
  );
}
function endPan(event?: PointerEvent) {
  const pointer = nodeDrag?.pointer ?? dragStart?.pointer;
  if (event && pointer !== undefined && event.pointerId !== pointer) return;
  if (nodeDrag?.moved) {
    clickSuppression = { id: nodeDrag.id, until: Date.now() + 450 };
  }
  const wasNodeDrag = Boolean(nodeDrag?.moved);
  nodeDrag = null;
  pinned = null;
  draggedId.value = null;
  dragStart = null;
  dragging.value = false;
  const captured = pointerCapture;
  pointerCapture = null;
  if (pointer !== undefined && captured?.hasPointerCapture?.(pointer)) {
    captured.releasePointerCapture(pointer);
  }
  if (wasNodeDrag) {
    if (reducedMotion.value) settleMotion();
    else wakeMotion();
  }
}

watch(opened, (value) => {
  if (value) {
    positions.value = new Map();
    rebuildMotion(true);
  } else {
    endPan();
    stopMotion();
  }
});
onMounted(() => {
  motionMedia = window.matchMedia("(prefers-reduced-motion: reduce)");
  updateMotionPreference();
  motionMedia.addEventListener("change", updateMotionPreference);
  document.addEventListener("visibilitychange", visibilityChanged);
  rebuildMotion();
});
onUnmounted(() => {
  disposed = true;
  endPan();
  stopMotion();
  motion = null;
  motionMedia?.removeEventListener("change", updateMotionPreference);
  document.removeEventListener("visibilitychange", visibilityChanged);
});
</script>
<template>
  <details
    class="profile-network"
    open
    @toggle="opened = ($event.target as HTMLDetailsElement).open"
  >
    <summary>本页关系图（最多50个人物，仅显示已批准的有向关系）</summary>
    <p>
      悬停或聚焦预览联系；点击、Enter或空格打开档案。拖动人物调整位置，拖动空白平移，滚轮缩放。
    </p>
    <div class="profile-tools" role="group" aria-label="关系图视图控制">
      <button :disabled="zoom >= 2.4" @click="zoomGraph(0.2)">放大</button
      ><button :disabled="zoom <= 0.5" @click="zoomGraph(-0.2)">缩小</button
      ><span
        role="meter"
        aria-label="关系图缩放比例"
        :aria-valuenow="Math.round(zoom * 100)"
        aria-valuemin="50"
        aria-valuemax="240"
        >{{ Math.round(zoom * 100) }}%</span
      >
      <button @click="pan.x -= 80">左移</button
      ><button @click="pan.x += 80">右移</button
      ><button @click="pan.y -= 80">上移</button
      ><button @click="pan.y += 80">下移</button
      ><button @click="resetGraph">复位</button>
    </div>
    <div
      class="graph-canvas"
      :class="{ dragging, 'dragging-node': draggedId }"
      :data-animating="animating"
      :data-reduced-motion="reducedMotion"
    >
      <svg
        ref="graphSvg"
        class="profile-graph"
        viewBox="0 0 900 680"
        role="group"
        aria-label="当前页已批准人物关系图"
        @wheel.prevent="zoomGraph($event.deltaY < 0 ? 0.1 : -0.1)"
        @pointerdown="startPan"
        @pointermove="movePan"
        @pointerup="endPan"
        @pointercancel="endPan"
        @lostpointercapture="endPan"
      >
        <defs>
          <marker
            :id="'profile-arrow-' + uid"
            markerWidth="8"
            markerHeight="8"
            refX="7"
            refY="4"
            orient="auto"
            markerUnits="userSpaceOnUse"
          >
            <path
              d="M1,1 L7,4 L1,7"
              fill="none"
              stroke="context-stroke"
              stroke-width="1.3"
            />
          </marker>
        </defs>
        <g
          ref="graphViewport"
          class="profile-graph-viewport"
          :transform="`translate(${450 + pan.x} ${340 + pan.y}) scale(${renderedZoom}) translate(-450 -340)`"
        >
          <g
            v-for="edge in graphEdges"
            :key="edge.id"
            class="profile-graph-edge"
            :class="{
              highlighted: isConnected(edge),
              dimmed: highlightedId && !isConnected(edge),
            }"
            :data-edge-id="edge.id"
          >
            <path
              :d="edgePath(edge)"
              :marker-end="'url(#profile-arrow-' + uid + ')'"
            />
            <title>
              {{ name(edge.from_person_id) }} → {{ name(edge.to_person_id) }} ·
              {{ edge.relationship_type }}
            </title>
            <text
              v-if="isConnected(edge)"
              :x="
                (positions.get(edge.from_person_id)!.x +
                  positions.get(edge.to_person_id)!.x) /
                2
              "
              :y="
                (positions.get(edge.from_person_id)!.y +
                  positions.get(edge.to_person_id)!.y) /
                  2 -
                9
              "
            >
              {{ edge.relationship_type }}
            </text>
          </g>
          <g
            v-for="person in graphPeople"
            :key="person.id"
            class="profile-graph-node"
            :class="{
              selected: selectedId === person.id,
              hovered: hoveredId === person.id || focusedId === person.id,
              dragged: draggedId === person.id,
              neighbor: highlightedId && connectedIds.has(person.id),
              dimmed:
                highlightedId &&
                !connectedIds.has(person.id) &&
                selectedId !== person.id,
            }"
            :transform="`translate(${positions.get(person.id)!.x},${positions.get(person.id)!.y})`"
            role="button"
            tabindex="0"
            :aria-label="'查看' + person.data.name + '的资料'"
            :aria-pressed="selectedId === person.id"
            :aria-disabled="busy"
            :data-person-id="person.id"
            @pointerdown.stop="startNodeDrag($event, person.id)"
            @pointerenter="hoveredId = person.id"
            @pointerleave="hoveredId = null"
            @focus="focusedId = person.id"
            @blur="focusedId = null"
            @click.stop="activatePerson(person)"
            @keydown.enter.prevent="!busy && choose(person)"
            @keydown.space.prevent="!busy && choose(person)"
          >
            <circle
              class="node-halo"
              :r="positions.get(person.id)!.radius + 6"
            />
            <circle
              class="node-disc"
              :r="positions.get(person.id)!.radius"
              :style="{
                '--node-opacity': String(
                  0.36 + Math.min(positions.get(person.id)!.degree, 7) * 0.08,
                ),
              }"
            />
            <text text-anchor="middle" dominant-baseline="middle">
              {{ label(person) }}
            </text>
            <title>
              {{ person.data.name }} · {{ degree(person.id) }} 条已批准关系
            </title>
          </g>
        </g>
      </svg>
      <p v-if="!graphPeople.length" class="graph-empty">
        当前页没有未合并人物。
      </p>
    </div>
    <p>
      圆点大小只表示当前页关联数量。箭头表示单向关系，间接联系不代表直接认识；拖动不修改人物或关系。
    </p>
  </details>
</template>
<style scoped>
.profile-network {
  margin: 18px 0;
  min-width: 0;
}
.profile-tools {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
  margin: 12px 0;
}
.graph-canvas {
  position: relative;
  min-width: 0;
  overflow: hidden;
  border: 1px solid #aab3bf;
  border-radius: 10px;
  background: radial-gradient(ellipse at center, #f7f7fa, #eef1f7);
}
.profile-graph {
  display: block;
  width: 100%;
  height: 500px;
  touch-action: none;
  user-select: none;
  cursor: grab;
}
.dragging .profile-graph,
.dragging-node .profile-graph {
  cursor: grabbing;
}
.profile-graph-node {
  cursor: pointer;
  outline: none;
}
.profile-graph-node.dragged {
  cursor: grabbing;
}
.node-disc {
  fill: rgba(98, 117, 156, var(--node-opacity, 0.4));
  stroke: #5f7293;
  stroke-width: 1.5;
}
.node-halo {
  fill: transparent;
  stroke: transparent;
}
.profile-graph-node.selected .node-disc {
  fill: #d3bf85;
  stroke: #816d31;
  stroke-width: 2.3;
}
.profile-graph-node.hovered .node-halo,
.profile-graph-node:focus-visible .node-halo {
  stroke: #526daa;
  stroke-width: 2;
}
.profile-graph-node.neighbor .node-disc {
  stroke: #526daa;
  stroke-width: 2;
}
.profile-graph-node.dimmed {
  opacity: 0.25;
}
.profile-graph-edge {
  color: #7989a6;
  pointer-events: none;
}
.profile-graph-edge path {
  fill: none;
  stroke: currentColor;
  stroke-width: 1.25;
  opacity: 0.55;
}
.profile-graph-edge.highlighted {
  color: #536fa6;
}
.profile-graph-edge.highlighted path {
  opacity: 1;
  stroke-width: 2;
}
.profile-graph-edge.dimmed {
  opacity: 0.16;
}
.profile-graph text {
  fill: #29384d;
  font-size: 12px;
  pointer-events: none;
}
.profile-graph-edge text {
  text-anchor: middle;
  paint-order: stroke;
  stroke: #f3f5f9;
  stroke-width: 3px;
  stroke-linejoin: round;
}
.graph-empty {
  position: absolute;
  top: 40%;
  left: 0;
  right: 0;
  text-align: center;
  pointer-events: none;
}
.profile-network p {
  overflow-wrap: anywhere;
}
@media (max-width: 760px) {
  .profile-graph {
    height: 340px;
  }
}
@media (prefers-reduced-motion: reduce) {
  * {
    transition: none !important;
    animation: none !important;
  }
}
</style>
