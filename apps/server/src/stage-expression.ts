const expressions = {
  blush: "害羞或被夸奖时的脸红",
  love: "明确的喜爱或惊喜",
  dizzy: "困惑或晕头转向",
  tears: "伤心或感动落泪",
  shadow: "阴沉或无语",
  sweat: "尴尬或紧张",
  smug: "得意或俏皮",
  squint: "开心眯眼",
  loading: "认真思考",
  buckteeth: "俏皮露兔牙",
  neutral: "主动恢复默认脸部表情",
} as const;

export type StageExpressionId = keyof typeof expressions;

export function isStageExpressionId(
  value: unknown,
): value is StageExpressionId {
  return typeof value === "string" && Object.hasOwn(expressions, value);
}

const marker = "PROMETHEUS_STAGE_FACE_V1";
const markerMention = new RegExp(marker, "iu");
const inlineControl = new RegExp(`${marker}(?:[\\t ]+[a-z0-9_-]+)?`, "giu");

export const stageExpressionPrompt = `
宿主本机舞台规则：你可以根据本轮回复的情绪自主选择一个脸部表情。先完整写出正常回复，保留本轮要求的格式（包括完整 JSON）。若适合，在回复正文之外另起最后一行，格式为 ${marker} <ID>，只填写一个下列 ID。没有合适表情时省略这行。不要在正文提到这个标记，也不要添加其他参数；这只改变本机舞台短暂的脸部表情。
${Object.entries(expressions)
  .map(([id, description]) => `${id}=${description}`)
  .join("；")}`;

export function parseStageExpression(content: string): {
  content: string;
  action: StageExpressionId | null;
} {
  if (!markerMention.test(content)) return { content, action: null };
  // Preserve line endings and the complete body, including a structured reply.
  const lines = content.split(/(?<=\n)/u);
  const isFooter = (line: string) =>
    line
      .replace(/^[\t >`*#_+-]*/u, "")
      .toUpperCase()
      .startsWith(marker);
  const footers = lines.filter(isFooter);

  const footer = footers[0];
  const exact = footer?.trimEnd();
  const id = exact?.startsWith(`${marker} `)
    ? exact.slice(marker.length + 1)
    : null;
  const body = lines
    .filter((line) => !isFooter(line))
    .join("")
    .trimEnd();
  const action =
    body &&
    !markerMention.test(body) &&
    footers.length === 1 &&
    lines.findLast((line) => line.trim()) === footer &&
    isStageExpressionId(id)
      ? id
      : null;
  // A malformed inline mention is never a command. Remove the reserved token
  // and its ID without dropping the containing text or its JSON punctuation.
  return { content: body.replace(inlineControl, "").trimEnd(), action };
}
