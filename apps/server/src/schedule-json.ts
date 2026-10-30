import { bad } from "./errors.ts";
/** Parse one bounded JSON value, rejecting duplicate keys instead of silently keeping the last. */
export function strictModelJson(input: string): unknown {
  let text = input.trim();
  if (text.startsWith("```")) {
    const match = /^```json\s*\r?\n([\s\S]*?)\r?\n```$/.exec(text);
    if (!match) throw bad("规划输出不是完整JSON", "planning_invalid_json");
    text = match[1]!.trim();
  }
  let at = 0,
    nodes = 0;
  const space = () => {
    while (/[\x20\t\r\n]/.test(text[at] ?? "x")) at++;
  };
  const quoted = () => {
    space();
    if (text[at] !== '"') throw Error();
    const start = at++;
    while (at < text.length) {
      if (text[at] === '"') {
        at++;
        return JSON.parse(text.slice(start, at)) as string;
      }
      if (text[at] === "\\") at += 2;
      else at++;
    }
    throw Error();
  };
  const value = (depth: number): void => {
    if (depth > 16 || ++nodes > 5000) throw Error();
    space();
    const next = text[at];
    if (next === '"') {
      quoted();
      return;
    }
    if (next === "{" || next === "[") {
      at++;
      const end = next === "{" ? "}" : "]",
        seen = new Set<string>();
      space();
      if (text[at] === end) {
        at++;
        return;
      }
      while (at < text.length) {
        if (next === "{") {
          const key = quoted();
          if (seen.has(key)) throw Error();
          seen.add(key);
          space();
          if (text[at++] !== ":") throw Error();
        }
        value(depth + 1);
        space();
        if (text[at] === end) {
          at++;
          return;
        }
        if (text[at++] !== ",") throw Error();
      }
      throw Error();
    }
    const match =
      /^(?:true|false|null|-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(
        text.slice(at),
      );
    if (!match) throw Error();
    at += match[0].length;
  };
  try {
    value(0);
    space();
    if (at !== text.length) throw Error();
    return JSON.parse(text);
  } catch {
    throw bad(
      "规划输出包含无效JSON、重复字段或额外内容",
      "planning_invalid_json",
    );
  }
}
