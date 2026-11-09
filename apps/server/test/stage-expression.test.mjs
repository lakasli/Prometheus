import assert from "node:assert/strict";
import { resolve } from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

const sourceRoot = fileURLToPath(new URL("../../../", import.meta.url));
const runtimeRoot = resolve(process.env.PROMETHEUS_RELEASE_ROOT ?? sourceRoot);
const { isStageExpressionId, parseStageExpression } = await import(
  pathToFileURL(resolve(runtimeRoot, "apps/server/dist/stage-expression.js"))
);

const marker = "PROMETHEUS_STAGE_FACE_V1";

test("stage expression sidecar preserves ordinary text and structured bodies", () => {
  const plain = "自然回复\r\n下一行\n\n";
  assert.deepEqual(parseStageExpression(plain), {
    content: plain,
    action: null,
  });
  const structured = '{\r\n  "reply":"谢谢",\r\n  "self_activity":null\r\n}';
  assert.deepEqual(
    parseStageExpression(`${structured}\r\n${marker} blush\r\n`),
    {
      content: structured,
      action: "blush",
    },
  );
  assert.deepEqual(parseStageExpression(`好的\n${marker} neutral`), {
    content: "好的",
    action: "neutral",
  });
});

test("stage expression sidecar drops invalid or ambiguous control lines", () => {
  for (const sidecar of [
    `${marker} frames`,
    `${marker} ../../model`,
    `${marker} blush duration=999999`,
    `${marker} blush love`,
    `${marker} __proto__`,
    ` ${marker} blush`,
    `\`${marker} blush\``,
    `- **${marker} blush**`,
    `> ${marker.toLowerCase()} blush`,
    `${marker} blush\n${marker} love`,
  ]) {
    assert.deepEqual(parseStageExpression(`正常回复\n${sidecar}`), {
      content: "正常回复",
      action: null,
    });
  }
  assert.deepEqual(parseStageExpression(`${marker} blush\n正常回复`), {
    content: "正常回复",
    action: null,
  });
  assert.deepEqual(parseStageExpression(`${marker} blush`), {
    content: "",
    action: null,
  });
  assert.equal(isStageExpressionId(null), false);
  assert.equal(isStageExpressionId(["blush"]), false);
});

test("stage expression sidecar cannot leak through inline text or structured replies", () => {
  assert.deepEqual(parseStageExpression(`谢谢 ${marker} blush`), {
    content: "谢谢",
    action: null,
  });
  assert.deepEqual(
    parseStageExpression(`{"reply":"谢谢 ${marker} blush"}\n${marker} love`),
    { content: '{"reply":"谢谢 "}', action: null },
  );
});
