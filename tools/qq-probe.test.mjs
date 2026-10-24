import assert from 'node:assert/strict';
import test from 'node:test';
import { boundedJson, gatewayMetadata, numericCode, ProbeError } from './qq-probe.mjs';

test('diagnostic metadata only exposes numeric codes and a redacted hostname', () => {
  assert.equal(numericCode({ code: '4004', message: 'DO_NOT_EXPOSE' }), 4004);
  assert.equal(numericCode({ code: 'DO_NOT_EXPOSE' }), null);
  const value = gatewayMetadata('wss://123456.sgroup.qq.com/websocket?token=DO_NOT_EXPOSE', ['123456', 'DO_NOT_EXPOSE']);
  assert.equal(value.allowed, true);
  assert.equal(value.host, '[redacted].sgroup.qq.com');
  assert.ok(!JSON.stringify(value).includes('DO_NOT_EXPOSE'));
  assert.ok(!JSON.stringify(value).includes('123456'));
});

test('gateway metadata rejects unrelated domains and credentials', () => {
  assert.equal(gatewayMetadata('wss://api.sgroup.qq.com/websocket').allowed, true);
  assert.equal(gatewayMetadata('wss://api.bot.qq.com/websocket').allowed, true);
  for (const url of ['ws://api.sgroup.qq.com', 'wss://api.sgroup.qq.com.attacker.invalid', 'wss://user:password@api.sgroup.qq.com', 'not-a-url']) assert.equal(gatewayMetadata(url).allowed, false);
});

test('HTTP response parsing is bounded and never includes raw JSON errors', async () => {
  assert.deepEqual(await boundedJson(new Response('{"code":0}')), { code: 0 });
  await assert.rejects(boundedJson(new Response('SECRET-NOT-JSON')), error => error instanceof ProbeError && error.code === 'response_not_json' && !error.message.includes('SECRET'));
  await assert.rejects(boundedJson(new Response('a'.repeat(65537))), error => error instanceof ProbeError && error.code === 'response_too_large');
  await assert.rejects(boundedJson(new Response('{}', { headers: { 'content-length': '65537' } })), error => error instanceof ProbeError && error.code === 'response_too_large');
});
