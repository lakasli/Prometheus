export interface RelationshipGraphNode {
  x: number;
  y: number;
  radius: number;
  degree: number;
}

interface GraphEdge {
  id: string;
  from_person_id: string;
  to_person_id: string;
}

const WIDTH = 900;
const HEIGHT = 680;
const MARGIN = 26;

function hash(text: string) {
  let result = 2166136261;
  for (let i = 0; i < text.length; i++)
    result = Math.imul(result ^ text.charCodeAt(i), 16777619);
  return result >>> 0;
}

function constrain(node: RelationshipGraphNode) {
  node.x = Math.min(
    WIDTH - MARGIN - node.radius,
    Math.max(MARGIN + node.radius, node.x),
  );
  node.y = Math.min(
    HEIGHT - MARGIN - node.radius,
    Math.max(MARGIN + node.radius, node.y),
  );
}

/** A bounded, deterministic layout: no animation loop or persistent worker. */
export function layoutRelationshipGraph(
  people: { id: string }[],
  edges: GraphEdge[],
): Map<string, RelationshipGraphNode> {
  const ids = [...new Set(people.map((person) => person.id))]
    .sort()
    .slice(0, 50);
  const index = new Map(ids.map((id, i) => [id, i]));
  const links = edges
    .filter(
      (edge) => index.has(edge.from_person_id) && index.has(edge.to_person_id),
    )
    .sort((a, b) => a.id.localeCompare(b.id));
  const degree = ids.map(() => 0);
  for (const link of links) {
    degree[index.get(link.from_person_id)!]++;
    if (link.to_person_id !== link.from_person_id)
      degree[index.get(link.to_person_id)!]++;
  }
  const nodes = ids.map((id, i) => {
    const angle = i * Math.PI * (3 - Math.sqrt(5)) + (hash(id) % 100) / 400;
    const spread = Math.sqrt((i + 0.5) / Math.max(1, ids.length));
    return {
      x: WIDTH / 2 + Math.cos(angle) * spread * 350,
      y: HEIGHT / 2 + Math.sin(angle) * spread * 250,
      radius: Math.min(48, 22 + Math.sqrt(degree[i]) * 5),
      degree: degree[i],
    };
  });
  if (nodes.length === 1) {
    nodes[0].x = WIDTH / 2;
    nodes[0].y = HEIGHT / 2;
  }
  for (let step = 0; step < 360; step++) {
    const force = nodes.map((node) => ({
      x: (WIDTH / 2 - node.x) * 0.003,
      y: (HEIGHT / 2 - node.y) * 0.003,
    }));
    for (let a = 0; a < nodes.length; a++) {
      for (let b = a + 1; b < nodes.length; b++) {
        const dx = nodes[b].x - nodes[a].x;
        const dy = nodes[b].y - nodes[a].y;
        const distance = Math.max(1, Math.hypot(dx, dy));
        const strength = 1200 / (distance * distance);
        force[a].x -= (dx / distance) * strength;
        force[a].y -= (dy / distance) * strength;
        force[b].x += (dx / distance) * strength;
        force[b].y += (dy / distance) * strength;
      }
    }
    for (const link of links) {
      const a = index.get(link.from_person_id)!;
      const b = index.get(link.to_person_id)!;
      if (a === b) continue;
      const dx = nodes[b].x - nodes[a].x;
      const dy = nodes[b].y - nodes[a].y;
      const distance = Math.max(1, Math.hypot(dx, dy));
      const strength =
        (distance - nodes[a].radius - nodes[b].radius - 65) * 0.018;
      force[a].x += (dx / distance) * strength;
      force[a].y += (dy / distance) * strength;
      force[b].x -= (dx / distance) * strength;
      force[b].y -= (dy / distance) * strength;
    }
    nodes.forEach((node, i) => {
      node.x += Math.max(-7, Math.min(7, force[i].x));
      node.y += Math.max(-7, Math.min(7, force[i].y));
      constrain(node);
    });
    separate(nodes, 2);
  }
  // Final collision relaxation is independent of springs, including at walls.
  separate(nodes, 400);
  expandToCanvas(nodes);
  return new Map(ids.map((id, i) => [id, nodes[i]]));
}

function expandToCanvas(nodes: RelationshipGraphNode[]) {
  if (nodes.length < 2) return;
  const minX = Math.min(...nodes.map((node) => node.x));
  const maxX = Math.max(...nodes.map((node) => node.x));
  const minY = Math.min(...nodes.map((node) => node.y));
  const maxY = Math.max(...nodes.map((node) => node.y));
  const radius = Math.max(...nodes.map((node) => node.radius));
  // Fit sparse networks to the viewport without enlarging circles or shrinking
  // dense layouts. Uniform expansion preserves relative positions and spacing.
  const scale = Math.min(
    (WIDTH * 0.84 - radius * 2) / Math.max(1, maxX - minX),
    (HEIGHT * 0.84 - radius * 2) / Math.max(1, maxY - minY),
  );
  if (scale <= 1) return;
  const centerX = (minX + maxX) / 2;
  const centerY = (minY + maxY) / 2;
  for (const node of nodes) {
    node.x = WIDTH / 2 + (node.x - centerX) * scale;
    node.y = HEIGHT / 2 + (node.y - centerY) * scale;
  }
}

function separate(nodes: RelationshipGraphNode[], rounds: number) {
  for (let round = 0; round < rounds; round++) {
    let overlap = false;
    for (let a = 0; a < nodes.length; a++) {
      for (let b = a + 1; b < nodes.length; b++) {
        let dx = nodes[b].x - nodes[a].x;
        let dy = nodes[b].y - nodes[a].y;
        if (dx === 0 && dy === 0) {
          dx = Math.cos(a + b);
          dy = Math.sin(a + b);
        }
        const distance = Math.hypot(dx, dy);
        const minimum = nodes[a].radius + nodes[b].radius + 12;
        if (distance >= minimum - 0.01) continue;
        overlap = true;
        const shift = (minimum - distance) / 2;
        nodes[a].x -= (dx / distance) * shift;
        nodes[a].y -= (dy / distance) * shift;
        nodes[b].x += (dx / distance) * shift;
        nodes[b].y += (dy / distance) * shift;
        constrain(nodes[a]);
        constrain(nodes[b]);
      }
    }
    if (!overlap) break;
  }
}

/** Clip arrows to circle boundaries; opposite directed edges use separate arcs. */
export function graphEdgePath(
  edgeId: string,
  from: RelationshipGraphNode,
  to: RelationshipGraphNode,
  reciprocal = false,
): string {
  const point = (x: number, y: number) => `${x.toFixed(2)} ${y.toFixed(2)}`;
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const distance = Math.hypot(dx, dy);
  if (distance < 1) {
    const side = hash(edgeId) % 2 ? 1 : -1;
    const r = from.radius;
    return `M ${point(from.x + side * r * 0.7, from.y - r * 0.7)} C ${point(from.x + side * (r + 72), from.y - r - 70)} ${point(from.x - side * (r + 72), from.y - r - 70)} ${point(from.x - side * (r + 8) * 0.7, from.y - (r + 8) * 0.7)}`;
  }
  const bend = reciprocal ? Math.min(65, distance * 0.2) : 0;
  const cx = (from.x + to.x) / 2 - (dy / distance) * bend;
  const cy = (from.y + to.y) / 2 + (dx / distance) * bend;
  const fromDistance = Math.hypot(cx - from.x, cy - from.y);
  const toDistance = Math.hypot(cx - to.x, cy - to.y);
  const start = point(
    from.x + ((cx - from.x) / fromDistance) * (from.radius + 2),
    from.y + ((cy - from.y) / fromDistance) * (from.radius + 2),
  );
  const end = point(
    to.x + ((cx - to.x) / toDistance) * (to.radius + 8),
    to.y + ((cy - to.y) / toDistance) * (to.radius + 8),
  );
  return reciprocal
    ? `M ${start} Q ${point(cx, cy)} ${end}`
    : `M ${start} L ${end}`;
}
