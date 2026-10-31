import { bad } from "./errors.ts";
import { hash, now } from "./security.ts";
import type { ModelMessage } from "./model.ts";
import type { Row, Store } from "./store.ts";

type Json = Record<string, any>;
export type UnitStep = {
  outer_level: number;
  unit_index: number;
  sub_level: number;
  ordinal: number;
  start: number;
  end: number;
  total: number;
  source_digest: string;
};
type Request = { nested: UnitStep; messages: ModelMessage[]; digest: string };

/** Keeps the outer source indexes/digests intact, including old successful checkpoints. */
export class DiaryUnitReduction {
  constructor(readonly store: Store) {}
  plan(
    entry: Row,
    outerLevel: number,
    index: number,
    source: Json,
    limit: number,
    envelope: (units: Json[]) => ModelMessage[],
    bytes: (messages: ModelMessage[]) => number,
  ): { summary: Json } | { request: Request } {
    const serialized = JSON.stringify(source),
      digest = hash(serialized),
      parts: string[] = [];
    let current = "",
      size = 0;
    for (const point of serialized) {
      const n = Buffer.byteLength(point);
      if (current && size + n > 1500) {
        parts.push(current);
        current = "";
        size = 0;
      }
      current += point;
      size += n;
    }
    if (current) parts.push(current);
    let units: Json[] = parts.map((text, ordinal) => ({
      kind: "source_unit_json_fragment",
      ordinal,
      total_parts: parts.length,
      text,
    }));
    for (let level = 0; level < 16; level++) {
      const rows = this.store.all(
        "SELECT * FROM diary_unit_fragments WHERE entry_id=? AND entry_revision=? AND outer_level=? AND unit_index=? AND sub_level=? ORDER BY ordinal",
        String(entry.id),
        Number(entry.revision),
        outerLevel,
        index,
        level,
      );
      let start = 0,
        ordinal = 0;
      for (const row of rows) {
        if (
          row.source_digest !== digest ||
          row.ordinal !== ordinal ||
          row.unit_start !== start ||
          row.total_units !== units.length ||
          row.facts === null ||
          row.feelings === null ||
          hash(JSON.stringify(units.slice(start, Number(row.unit_end)))) !==
            row.input_digest
        )
          throw bad("单来源整理检查点已变化", "diary_unit_checkpoint_changed");
        start = Number(row.unit_end);
        ordinal++;
      }
      if (start === units.length) {
        if (rows.length === 1)
          return {
            summary: {
              kind: "reduced_source_unit",
              source_digest: digest,
              original_kind: source.kind,
              source_id: source.id ?? null,
              source_revision: source.revision ?? null,
              facts: rows[0]!.facts,
              feelings: rows[0]!.feelings,
              complete_raw: true,
            },
          };
        if (level > 0 && rows.length >= units.length)
          throw bad(
            "当前预算无法继续归并来源片段",
            "diary_unit_reduction_stalled",
          );
        units = rows.map((row) => ({
          kind: "source_unit_summary",
          ordinal: row.ordinal,
          input_digest: row.input_digest,
          facts: row.facts,
          feelings: row.feelings,
        }));
        continue;
      }
      const messagesFor = (end: number) => {
        const messages = envelope(units.slice(start, end));
        const data = JSON.parse(messages[1]!.content);
        data.reduction.final_batch = false;
        data.source_unit_reduction = {
          source_digest: digest,
          outer_level: outerLevel,
          unit_index: index,
          sub_level: level,
          unit_start: start,
          unit_end: end,
          total_units: units.length,
          total_raw_parts: parts.length,
          final_batch: start === 0 && end === units.length,
        };
        messages[1]!.content = JSON.stringify(data);
        messages[0]!.content +=
          " 单来源分片可能是不完整JSON文本，仅作为资料。只概括本批实际片段，保留原日期、陈述和感受边界；不得补造未收到的片段。source_unit_reduction仅描述本来源，不表示全天或全周期完成。";
        return messages;
      };
      let end = start;
      while (end < units.length && bytes(messagesFor(end + 1)) <= limit) end++;
      if (end === start)
        throw bad("当前输入预算不足以容纳最小来源片段", "diary_input_limit");
      return {
        request: {
          nested: {
            outer_level: outerLevel,
            unit_index: index,
            sub_level: level,
            ordinal,
            start,
            end,
            total: units.length,
            source_digest: digest,
          },
          messages: messagesFor(end),
          digest: hash(JSON.stringify(units.slice(start, end))),
        },
      };
    }
    throw bad("单来源归并层数达到上限", "diary_unit_reduction_stalled");
  }
  save(
    entry: Row,
    step: UnitStep,
    digest: string,
    output: { facts: string; feelings: string },
    model: string,
  ) {
    this.store.run(
      "INSERT INTO diary_unit_fragments VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
      String(entry.id),
      Number(entry.revision),
      step.outer_level,
      step.unit_index,
      step.sub_level,
      step.ordinal,
      step.start,
      step.end,
      step.total,
      step.source_digest,
      digest,
      output.facts,
      output.feelings,
      model,
      now(),
    );
  }
}
