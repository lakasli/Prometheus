import { describe, it } from "node:test";
import { expect } from "@playwright/test";
import { layoutRelationshipGraph } from "../src/relationship-graph.ts";
import type { RelationshipGraphNode } from "../src/relationship-graph.ts";
import {
  createGraphMotion,
  snapshotGraphMotion,
  stepGraphMotion,
} from "../src/relationship-graph-motion.ts";
import type {
  GraphMotionState,
  GraphPin,
} from "../src/relationship-graph-motion.ts";

function node(x: number, y: number): RelationshipGraphNode {
  return { x, y, radius: 22, degree: 0 };
}

function settle(state: GraphMotionState, pin?: GraphPin) {
  for (let frame = 0; frame < 182; frame++) {
    const result = stepGraphMotion(state, pin);
    if (result.settled) return result.positions;
  }
  throw new Error("graph motion did not stop within its finite frame budget");
}

function withinCanvas(positions: Map<string, RelationshipGraphNode>) {
  for (const n of positions.values()) {
    expect([n.x, n.y, n.radius, n.degree].every(Number.isFinite)).toBe(true);
    expect(n.x - n.radius).toBeGreaterThanOrEqual(26);
    expect(n.x + n.radius).toBeLessThanOrEqual(874);
    expect(n.y - n.radius).toBeGreaterThanOrEqual(26);
    expect(n.y + n.radius).toBeLessThanOrEqual(654);
  }
}

describe("bounded relationship graph motion", () => {
  it("moves a real neighbor with a dragged node while a disconnected person stays still", () => {
    const layout = new Map([
      ["a", node(250, 340)],
      ["b", node(450, 340)],
      ["unconnected", node(720, 500)],
    ]);
    const state = createGraphMotion(layout, [
      { id: "reviewed", from_person_id: "a", to_person_id: "b" },
    ]);
    const positions = settle(state, { id: "a", x: 320, y: 340 });
    expect(positions.get("a")!.x).toBe(320);
    expect(positions.get("b")!.x).toBeGreaterThan(470);
    expect(positions.get("unconnected")).toEqual(layout.get("unconnected"));
    expect(state.nodes.get("a")!.targetX).toBe(320);
    const released = settle(state);
    expect(state.nodes.get("a")!.targetX).toBe(320);
    expect(released.get("a")!.x).toBeGreaterThan(270);
    expect(released.get("b")!.x).toBeGreaterThan(460);
    withinCanvas(released);
  });

  it("restarts only when a pin moves or is released, and exposes isolated snapshots", () => {
    const state = createGraphMotion(new Map([["a", node(450, 340)]]), []);
    const initial = snapshotGraphMotion(state);
    initial.get("a")!.x = 0;
    expect(state.nodes.get("a")!.x).toBe(450);
    settle(state);
    expect(state.settled).toBe(true);
    const quiet = stepGraphMotion(state);
    expect(quiet.settled).toBe(true);
    const moved = stepGraphMotion(state, { id: "a", x: 500, y: 340 });
    expect(moved.settled).toBe(false);
    expect(moved.positions.get("a")!.x).toBe(500);
    settle(state, { id: "a", x: 500, y: 340 });
    expect(stepGraphMotion(state, { id: "a", x: 500, y: 340 }).settled).toBe(
      true,
    );
    expect(stepGraphMotion(state).settled).toBe(false);
    settle(state);
    expect(state.nodes.get("a")!.vx).toBe(0);
    expect(state.nodes.get("a")!.vy).toBe(0);
    expect(snapshotGraphMotion(state)).not.toBe(quiet.positions);
  });

  it("keeps a maximum of fifty nodes, sanitizes malformed coordinates, and caps frame velocity", () => {
    const layout = new Map(
      Array.from({ length: 65 }, (_, i) => [
        `person-${String(i).padStart(2, "0")}`,
        node(i * 80, Number.NaN),
      ]),
    );
    layout.set("person-00", {
      x: Number.POSITIVE_INFINITY,
      y: Number.NaN,
      radius: Number.NaN,
      degree: Number.NaN,
    });
    const previous = new Map(
      [...layout].map(([id]) => [id, node(-1e12, 1e12)]),
    );
    const state = createGraphMotion(layout, [], previous);
    expect(state.nodes.size).toBe(50);
    for (let i = 0; i < 12; i++) {
      const result = stepGraphMotion(
        state,
        { id: "person-00", x: Number.NaN, y: Number.POSITIVE_INFINITY },
        i ? 1e12 : Number.NaN,
      );
      withinCanvas(result.positions);
      for (const n of state.nodes.values()) {
        expect(Number.isFinite(n.vx) && Number.isFinite(n.vy)).toBe(true);
        expect(Math.hypot(n.vx, n.vy)).toBeLessThanOrEqual(8.000001);
      }
    }
    withinCanvas(settle(state));
    expect(createGraphMotion(new Map(), []).settled).toBe(true);
    expect(
      stepGraphMotion(createGraphMotion(new Map(), [])).positions.size,
    ).toBe(0);
  });

  it("resolves coincident circles without moving a fixed node or producing NaN", () => {
    const layout = new Map([
      ["a", node(300, 300)],
      ["b", node(600, 400)],
    ]);
    const previous = new Map([
      ["a", node(450, 340)],
      ["b", node(450, 340)],
    ]);
    const state = createGraphMotion(layout, [], previous);
    const result = stepGraphMotion(state, { id: "a", x: 450, y: 340 });
    const a = result.positions.get("a")!;
    const b = result.positions.get("b")!;
    expect(a.x).toBe(450);
    expect(a.y).toBe(340);
    expect(Math.hypot(b.x - a.x, b.y - a.y)).toBeGreaterThanOrEqual(
      a.radius + b.radius + 11.9,
    );
    withinCanvas(result.positions);
  });

  it("converges toward the layout, ignores removed endpoints, and is independent of API ordering", () => {
    const layout = new Map([
      ["a", node(200, 200)],
      ["b", node(450, 340)],
      ["c", node(700, 480)],
    ]);
    const previous = new Map(
      [...layout].map(([id, n]) => [id, { ...n, x: n.x - 30, y: n.y + 25 }]),
    );
    const edges = [
      { id: "1", from_person_id: "a", to_person_id: "b" },
      { id: "2", from_person_id: "b", to_person_id: "a" },
      { id: "3", from_person_id: "b", to_person_id: "missing" },
      { id: "4", from_person_id: "a", to_person_id: "a" },
    ];
    const first = createGraphMotion(layout, edges, previous);
    const second = createGraphMotion(
      new Map([...layout].reverse()),
      [...edges].reverse(),
      previous,
    );
    expect(first.links).toHaveLength(1);
    expect(settle(first)).toEqual(settle(second));
    for (const [id, n] of snapshotGraphMotion(first))
      expect(
        Math.hypot(n.x - layout.get(id)!.x, n.y - layout.get(id)!.y),
      ).toBeLessThan(3);
    expect(previous.get("a")).toEqual(node(170, 225));
    const disconnected = createGraphMotion(layout, []);
    const positions = settle(disconnected, { id: "a", x: 280, y: 200 });
    expect(positions.get("b")).toEqual(layout.get("b"));
  });

  it("finishes a fifty-person topology transition with separated, bounded circles", () => {
    const people = Array.from({ length: 50 }, (_, i) => ({ id: `p-${i}` }));
    const edges = people.slice(1).map((person, i) => ({
      id: `edge-${i}`,
      from_person_id: people[i].id,
      to_person_id: person.id,
    }));
    const layout = layoutRelationshipGraph(people, edges);
    const previous = new Map(
      [...layout].map(([id, n]) => [
        id,
        {
          ...n,
          x: 450 + (n.x - 450) * 0.92,
          y: 340 + (n.y - 340) * 0.92,
        },
      ]),
    );
    const positions = settle(createGraphMotion(layout, edges, previous));
    withinCanvas(positions);
    const nodes = [...positions.values()];
    for (let a = 0; a < nodes.length; a++)
      for (let b = a + 1; b < nodes.length; b++)
        expect(
          Math.hypot(nodes[b].x - nodes[a].x, nodes[b].y - nodes[a].y),
        ).toBeGreaterThanOrEqual(nodes[a].radius + nodes[b].radius + 11.8);
  });
});
