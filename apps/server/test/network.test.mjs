import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { fixture, until } from "./helpers.mjs";
test("model redirects are refused and their unfinished sockets close before releasing the request", async (t) => {
  let count = 0,
    closed = false;
  const provider = createServer(async (req, res) => {
    for await (const _chunk of req) {
      /* Consume request without logging it. */
    }
    count++;
    res.on("close", () => {
      closed = true;
    });
    res.writeHead(302, { location: "/must-not-follow" });
    res.flushHeaders();
  });
  await new Promise((resolve) => provider.listen(0, "127.0.0.1", resolve));
  t.after(
    () =>
      new Promise((resolve) => {
        provider.closeAllConnections();
        provider.close(resolve);
      }),
  );
  const app = await fixture();
  t.after(() => app.close());
  await app.bootstrap();
  await app.configure(`http://127.0.0.1:${provider.address().port}`);
  const conversation = (await app.call("/conversations", "POST", {})).data;
  const run = (
    await app.call(`/conversations/${conversation.id}/messages`, "POST", {
      content: "redirect-test",
      client_message_id: "redirect-test",
    })
  ).data;
  const result = await until(
    () => app.call(`/runs/${run.run_id}`),
    (value) => value.data.status === "failed",
  );
  assert.match(result.data.error, /HTTP 302/);
  await until(
    async () => closed,
    (value) => value,
  );
  assert.equal(count, 1);
  assert.equal(app.runtime.runningCount, 0);
});
