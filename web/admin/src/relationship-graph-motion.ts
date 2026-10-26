import type { RelationshipGraphNode } from "./relationship-graph";

interface MotionEdge {
  id: string;
  from_person_id: string;
  to_person_id: string;
}

export interface GraphPin {
  id: string;
  x: number;
  y: number;
}

interface MotionNode extends RelationshipGraphNode {
  vx: number;
  vy: number;
  targetX: number;
  targetY: number;
}

export interface GraphMotionState {
  nodes: Map<string, MotionNode>;
  links: { from: string; to: string; distance: number }[];
  elapsedMs: number;
  quietFrames: number;
  settled: boolean;
  lastPin: GraphPin | null;
}

const WIDTH = 900;
const HEIGHT = 680;
const MARGIN = 26;
const GAP = 12;
const FRAME_MS = 1000 / 60;
const MAX_SPEED = 8;
const MAX_DURATION_MS = 3000;

function finite(value: number, fallback: number) {
  return Number.isFinite(value) ? value : fallback;
}

function clamp(value: number, minimum: number, maximum: number) {
  return Math.min(maximum, Math.max(minimum, value));
}

function constrain(node: RelationshipGraphNode) {
  node.x = clamp(
    finite(node.x, WIDTH / 2),
    MARGIN + node.radius,
    WIDTH - MARGIN - node.radius,
  );
  node.y = clamp(
    finite(node.y, HEIGHT / 2),
    MARGIN + node.radius,
    HEIGHT - MARGIN - node.radius,
  );
}

function direction(from: string, to: string) {
  let hash = 2166136261;
  for (const character of `${from}\0${to}`)
    hash = Math.imul(hash ^ character.charCodeAt(0), 16777619);
  const angle = ((hash >>> 0) / 0xffffffff) * Math.PI * 2;
  return { x: Math.cos(angle), y: Math.sin(angle) };
}

/** Copy positions for a Vue shallowRef; callers never receive mutable velocities. */
export function snapshotGraphMotion(
  state: GraphMotionState,
): Map<string, RelationshipGraphNode> {
  return new Map(
    [...state.nodes].map(([id, node]) => [
      id,
      {
        x: node.x,
        y: node.y,
        radius: node.radius,
        degree: node.degree,
      },
    ]),
  );
}

/**
 * No timer or worker is created. Edges only influence existing, distinct endpoints;
 * reciprocal or repeated reviewed edges share one physical spring.
 */
export function createGraphMotion(
  layout: ReadonlyMap<string, RelationshipGraphNode>,
  edges: readonly MotionEdge[],
  previous?: ReadonlyMap<string, RelationshipGraphNode>,
): GraphMotionState {
  const nodes = new Map<string, MotionNode>();
  for (const id of [...layout.keys()].sort().slice(0, 50)) {
    const target = layout.get(id)!;
    const node: MotionNode = {
      x: finite(target.x, WIDTH / 2),
      y: finite(target.y, HEIGHT / 2),
      radius: clamp(finite(target.radius, 22), 8, 48),
      degree: Math.max(0, finite(target.degree, 0)),
      vx: 0,
      vy: 0,
      targetX: 0,
      targetY: 0,
    };
    constrain(node);
    node.targetX = node.x;
    node.targetY = node.y;
    const old = previous?.get(id);
    if (old) {
      node.x = finite(old.x, node.x);
      node.y = finite(old.y, node.y);
      constrain(node);
    }
    nodes.set(id, node);
  }
  const pairs = new Map<
    string,
    { from: string; to: string; distance: number }
  >();
  for (const edge of edges) {
    const [from, to] = [edge.from_person_id, edge.to_person_id].sort();
    if (from === to || !nodes.has(from) || !nodes.has(to)) continue;
    const key = JSON.stringify([from, to]);
    const a = nodes.get(from)!;
    const b = nodes.get(to)!;
    pairs.set(key, {
      from,
      to,
      distance: Math.max(
        a.radius + b.radius + GAP,
        Math.hypot(b.targetX - a.targetX, b.targetY - a.targetY),
      ),
    });
  }
  return {
    nodes,
    links: [...pairs]
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
      .map(([, link]) => link),
    elapsedMs: 0,
    quietFrames: 0,
    settled: nodes.size === 0,
    lastPin: null,
  };
}

function separate(state: GraphMotionState, pinnedId?: string) {
  const entries = [...state.nodes];
  for (let round = 0; round < 16; round++) {
    let maximumOverlap = 0;
    for (let a = 0; a < entries.length; a++) {
      for (let b = a + 1; b < entries.length; b++) {
        const [aId, first] = entries[a];
        const [bId, second] = entries[b];
        let dx = second.x - first.x;
        let dy = second.y - first.y;
        let distance = Math.hypot(dx, dy);
        const minimum = first.radius + second.radius + GAP;
        const overlap = minimum - distance;
        if (overlap <= 0.01) continue;
        maximumOverlap = Math.max(maximumOverlap, overlap);
        if (distance < 0.001) {
          const unit = direction(aId, bId);
          dx = unit.x;
          dy = unit.y;
          distance = 1;
        }
        const firstShare = aId === pinnedId ? 0 : bId === pinnedId ? 1 : 0.5;
        const secondShare = bId === pinnedId ? 0 : aId === pinnedId ? 1 : 0.5;
        first.x -= (dx / distance) * overlap * firstShare;
        first.y -= (dy / distance) * overlap * firstShare;
        second.x += (dx / distance) * overlap * secondShare;
        second.y += (dy / distance) * overlap * secondShare;
        constrain(first);
        constrain(second);
      }
    }
    if (maximumOverlap <= 0.01) break;
  }
}

/**
 * Advance one bounded frame and return a fresh positions Map. Stop requesting
 * animation frames when settled. A moving pin wakes a stopped state; its last
 * location becomes that node's return target after release. Other nodes retain
 * their original layout targets, so only linked neighbors and collisions follow.
 */
export function stepGraphMotion(
  state: GraphMotionState,
  pinned?: GraphPin | null,
  deltaMs = FRAME_MS,
): { positions: Map<string, RelationshipGraphNode>; settled: boolean } {
  const pinNode = pinned ? state.nodes.get(pinned.id) : undefined;
  const pin =
    pinned && pinNode
      ? {
          id: pinned.id,
          x: clamp(
            finite(pinned.x, pinNode.x),
            MARGIN + pinNode.radius,
            WIDTH - MARGIN - pinNode.radius,
          ),
          y: clamp(
            finite(pinned.y, pinNode.y),
            MARGIN + pinNode.radius,
            HEIGHT - MARGIN - pinNode.radius,
          ),
        }
      : null;
  if (
    pin?.id !== state.lastPin?.id ||
    (pin &&
      state.lastPin &&
      Math.hypot(pin.x - state.lastPin.x, pin.y - state.lastPin.y) > 0.01)
  ) {
    state.elapsedMs = 0;
    state.quietFrames = 0;
    state.settled = false;
  }
  state.lastPin = pin;
  if (state.settled)
    return { positions: snapshotGraphMotion(state), settled: true };
  const dtMs = clamp(finite(deltaMs, FRAME_MS), 1, 40);
  const dt = dtMs / FRAME_MS;
  state.elapsedMs += dtMs;
  if (pin && pinNode) {
    pinNode.x = pinNode.targetX = pin.x;
    pinNode.y = pinNode.targetY = pin.y;
    pinNode.vx = pinNode.vy = 0;
  }
  const forces = new Map(
    [...state.nodes].map(([id, node]) => [
      id,
      {
        x: (node.targetX - node.x) * 0.008,
        y: (node.targetY - node.y) * 0.008,
      },
    ]),
  );
  for (const link of state.links) {
    const a = state.nodes.get(link.from)!;
    const b = state.nodes.get(link.to)!;
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const distance = Math.hypot(dx, dy);
    const unit =
      distance > 0.001
        ? { x: dx / distance, y: dy / distance }
        : direction(link.from, link.to);
    const spring = clamp((distance - link.distance) * 0.009, -2.4, 2.4);
    forces.get(link.from)!.x += unit.x * spring;
    forces.get(link.from)!.y += unit.y * spring;
    forces.get(link.to)!.x -= unit.x * spring;
    forces.get(link.to)!.y -= unit.y * spring;
  }
  const before = snapshotGraphMotion(state);
  for (const [id, node] of state.nodes) {
    if (id === pin?.id) continue;
    const force = forces.get(id)!;
    node.vx = finite((node.vx + force.x * dt) * 0.8 ** dt, 0);
    node.vy = finite((node.vy + force.y * dt) * 0.8 ** dt, 0);
    const speed = Math.hypot(node.vx, node.vy);
    if (speed > MAX_SPEED) {
      node.vx *= MAX_SPEED / speed;
      node.vy *= MAX_SPEED / speed;
    }
    node.x += node.vx * dt;
    node.y += node.vy * dt;
    constrain(node);
  }
  separate(state, pin?.id);
  let movement = 0;
  for (const [id, node] of state.nodes) {
    const old = before.get(id)!;
    movement = Math.max(movement, Math.hypot(node.x - old.x, node.y - old.y));
  }
  state.quietFrames = movement < 0.035 ? state.quietFrames + 1 : 0;
  state.settled = state.quietFrames >= 8 || state.elapsedMs >= MAX_DURATION_MS;
  if (state.settled)
    for (const node of state.nodes.values()) node.vx = node.vy = 0;
  return { positions: snapshotGraphMotion(state), settled: state.settled };
}
