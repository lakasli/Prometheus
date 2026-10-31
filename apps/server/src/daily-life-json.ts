import { bad, fields, integer, string } from "./errors.ts";
import { strictModelJson } from "./schedule-json.ts";
export interface TextSelection {
  slot_id: string;
  title: string;
  content: string;
}
export interface TextReplyPart {
  type: "text";
  text: string;
  delay_ms: number;
}
export function textPresentation(text: string): TextReplyPart[] {
  const chars = [...text];
  if (!chars.length || chars.length > 6000)
    throw bad("文字回复不能超过四项", "self_activity_invalid_reply");
  const result: TextReplyPart[] = [];
  for (let i = 0; i < chars.length; i += 1500)
    result.push({
      type: "text",
      text: chars.slice(i, i + 1500).join(""),
      delay_ms: i ? 1500 : 0,
    });
  return result;
}
export function textSelection(value: unknown): TextSelection | null {
  if (value === null) return null;
  const b = fields(value, ["slot_id", "title", "content"]),
    title = string(b.title, "文字安排名称", 160, 1).trim(),
    content = string(b.content, "完整文字成果", 12000, 1).trim();
  if (
    !title ||
    !content ||
    [...title].length > 80 ||
    [...content].length > 3000 ||
    Buffer.byteLength(content) > 12000 ||
    [title, content].some(
      (t) => t.includes("\0") || Buffer.from(t).toString("utf8") !== t,
    )
  )
    throw bad("文字成果超限或无效", "self_activity_invalid_text");
  return { slot_id: string(b.slot_id, "宿主时段", 64, 1), title, content };
}
function reply(value: unknown): {
  text: string;
  canonical: unknown;
  parts: TextReplyPart[] | null;
} {
  if (typeof value === "string") {
    const text = string(value, "回复", 32000, 1).trim();
    if (!text || Buffer.byteLength(text) > 32000) throw bad("回复为空或过长");
    return { text, canonical: text, parts: textPresentation(text) };
  }
  if (!Array.isArray(value) || value.length < 1 || value.length > 4)
    throw bad("文字回复须为1至4项", "self_activity_invalid_reply");
  const items = value.map((v) => {
    const b = fields(v, ["type", "text", "delay_ms"]);
    if (b.type !== "text")
      throw bad("此回复不能启用媒体或执行能力", "self_activity_invalid_reply");
    return {
      type: "text" as const,
      text: string(b.text, "回复文字", 1500, 1).trim(),
      delay_ms: integer(b.delay_ms ?? 0, "呈现间隔", 0, 30000),
    };
  });
  if (items.some((i) => !i.text)) throw bad("回复不能为空");
  const text = items.map((i) => i.text).join("\n");
  if (Buffer.byteLength(text) > 32000) throw bad("回复过长");
  return {
    text,
    canonical: items,
    parts: items.map((item, i) => ({
      ...item,
      delay_ms:
        i === 0 ? 0 : Math.max(1500, Math.min(8000, item.delay_ms || 1500)),
    })),
  };
}
function firstArrayEnd(text: string) {
  let depth = 0,
    quoted = false,
    escaped = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quoted) {
      if (escaped) escaped = false;
      else if (c === "\\") escaped = true;
      else if (c === '"') quoted = false;
      continue;
    }
    if (c === '"') quoted = true;
    else if (c === "[" || c === "{") depth++;
    else if ((c === "]" || c === "}") && --depth === 0) return i + 1;
  }
  throw bad("重复回复数组没有完整结束", "self_activity_invalid_reply");
}
export function splitTextPlan(raw: string) {
  let text = raw.trim(),
    leading: unknown;
  if (text.startsWith("[")) {
    const end = firstArrayEnd(text),
      prefix = text.slice(0, end),
      tail = text.slice(end).trim();
    if (tail) {
      leading = reply(strictModelJson(prefix)).canonical;
      text = tail;
    } else {
      const normalized = reply(strictModelJson(prefix));
      return {
        reply: normalized.text,
        parts: normalized.parts,
        selection: null,
        envelope: false,
      };
    }
  }
  const recognized =
    text.startsWith("{") || /^```(?:json)?[\t ]*\r?\n/.test(text);
  if (!recognized) {
    if (leading !== undefined) throw bad("重复回复后缺少唯一日程对象");
    return { reply: raw, parts: null, selection: null, envelope: false };
  }
  const parsed = strictModelJson(text);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    if (leading !== undefined) throw bad("重复回复后不是日程对象");
    return { reply: raw, parts: null, selection: null, envelope: false };
  }
  if (!("reply" in parsed) && !("self_activity" in parsed)) {
    if (leading !== undefined) throw bad("重复回复后缺少日程字段");
    return { reply: raw, parts: null, selection: null, envelope: false };
  }
  const b = fields(parsed, ["reply", "self_activity"]);
  if (!("reply" in b) || !("self_activity" in b))
    throw bad("日程对象缺少明确回复或决定", "self_activity_invalid_reply");
  const normalized = reply(b.reply);
  if (
    leading !== undefined &&
    JSON.stringify(leading) !== JSON.stringify(normalized.canonical)
  )
    throw bad("重复回复内容或字段不一致", "self_activity_reply_mismatch");
  return {
    reply: normalized.text,
    parts: normalized.parts,
    selection: textSelection(b.self_activity),
    envelope: true,
  };
}
/** A cheap request filter, never the authority that decides an intention. */
export function mayContainSelfIntention(reply: string) {
  return (
    /我/u.test(reply) &&
    /去|趁|准备|打算|先|现在|这会|待会|接下来|安排|写|读|整理/u.test(reply)
  );
}
