import { describe, expect, it } from "vitest";
import {
  graphEdgePath,
  layoutRelationshipGraph,
} from "../src/relationship-graph";
import type { RelationshipGraphNode } from "../src/relationship-graph";

function validate(nodes: Map<string, RelationshipGraphNode>) {
  const values = [...nodes.values()];
  for (const node of values) {
    expect(Number.isFinite(node.x) && Number.isFinite(node.y)).toBe(true);
    expect(node.x - node.radius).toBeGreaterThanOrEqual(26);
    expect(node.x + node.radius).toBeLessThanOrEqual(874);
    expect(node.y - node.radius).toBeGreaterThanOrEqual(26);
    expect(node.y + node.radius).toBeLessThanOrEqual(654);
  }
  for (let a = 0; a < values.length; a++)
    for (let b = a + 1; b < values.length; b++)
      expect(
        Math.hypot(values[a].x - values[b].x, values[a].y - values[b].y),
      ).toBeGreaterThanOrEqual(values[a].radius + values[b].radius + 11.9);
}

describe("relationship network layout", () => {
  it("keeps stable coordinates when API order changes and ignores missing endpoints", () => {
    const people = ["c", "a", "b"].map((id) => ({ id }));
    const edges = [
      { id: "1", from_person_id: "a", to_person_id: "b" },
      { id: "2", from_person_id: "a", to_person_id: "c" },
      { id: "3", from_person_id: "a", to_person_id: "missing" },
    ];
    const nodes = layoutRelationshipGraph(people, edges);
    expect(
      layoutRelationshipGraph([...people].reverse(), [...edges].reverse()),
    ).toEqual(nodes);
    expect(nodes.get("a")!.degree).toBe(2);
    expect(nodes.get("a")!.radius).toBeGreaterThan(nodes.get("b")!.radius);
    validate(nodes);
  });
  it("supports an empty graph and centers one isolated person", () => {
    expect(layoutRelationshipGraph([], []).size).toBe(0);
    expect(layoutRelationshipGraph([{ id: "only" }], []).get("only")).toEqual({
      x: 450,
      y: 340,
      degree: 0,
      radius: 22,
    });
  });
  it("keeps fifty connected or disconnected people apart inside the canvas", () => {
    const people = Array.from({ length: 50 }, (_, i) => ({
      id: `person-${i}`,
    }));
    const chain = people.slice(1).map((person, i) => ({
      id: `edge-${i}`,
      from_person_id: people[i].id,
      to_person_id: person.id,
    }));
    validate(layoutRelationshipGraph(people, chain));
    validate(layoutRelationshipGraph(people, []));
  });
  it("fits a small connected network across the canvas instead of a tiny central cluster", () => {
    const people = Array.from({ length: 10 }, (_, i) => ({ id: `p-${i}` }));
    const edges = people.slice(1).map((person, i) => ({
      id: `e-${i}`,
      from_person_id: people[Math.floor(i / 2)].id,
      to_person_id: person.id,
    }));
    const layout = layoutRelationshipGraph(people, edges);
    validate(layout);
    const nodes = [...layout.values()];
    const width =
      Math.max(...nodes.map((n) => n.x)) - Math.min(...nodes.map((n) => n.x));
    const height =
      Math.max(...nodes.map((n) => n.y)) - Math.min(...nodes.map((n) => n.y));
    expect(width > 500 || height > 400).toBe(true);
  });
  it("clips horizontal and vertical arrows outside the target circle", () => {
    const from = { x: 100, y: 100, radius: 22, degree: 1 };
    expect(graphEdgePath("horizontal", from, { ...from, x: 300 })).toBe(
      "M 124.00 100.00 L 270.00 100.00",
    );
    expect(graphEdgePath("vertical", from, { ...from, y: 300 })).toBe(
      "M 100.00 124.00 L 100.00 270.00",
    );
  });
  it("uses opposite arcs for reciprocal relationships and a loop for self edges", () => {
    const a = { x: 100, y: 100, radius: 22, degree: 2 };
    const b = { ...a, x: 300 };
    expect(graphEdgePath("a-b", a, b, true)).toContain("Q 200.00 140.00");
    expect(graphEdgePath("b-a", b, a, true)).toContain("Q 200.00 60.00");
    expect(graphEdgePath("self", a, a)).toContain(" C ");
    expect(graphEdgePath("self", a, a)).not.toContain("NaN");
  });
});
