// Only local HTTP/protocol fixtures. These are not external model evidence.
import test from 'node:test'
import assert from 'node:assert/strict'
import http from 'node:http'
import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { EventEmitter } from 'node:events'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { startInflightProxy } from '../inflight-proxy.mjs'

const key = 'unit-test-model-key'
const authorizationHash = createHash('sha256').update(`Bearer ${key}`).digest()
const directory = path.dirname(fileURLToPath(import.meta.url))

async function upstream(handler) {
  const server = http.createServer(handler)
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return {
    baseURL: `http://127.0.0.1:${server.address().port}/v1`,
    async close() {
      server.closeAllConnections()
      await new Promise(resolve => server.close(resolve))
    },
  }
}

test('IPC proxy mode cancels real pending HTTP without waiting for semantic chunks', async () => {
  let received = 0
  let secondAborted = false
  const service = await upstream((request, response) => {
    request.resume()
    request.on('end', () => {
      received++
      if (received === 1) response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ text: 'ACP_M0_OK' }))
      // Keep the second upstream HTTP response unfinished, like a provider
      // that has not yet committed its semantic response.
      else response.on('close', () => { secondAborted = true })
    })
  })
  const proxy = await startInflightProxy({ upstreamBaseURL: service.baseURL, authorizationHash, timeoutMs: 5000 })
  try {
    const outcome = await new Promise((resolve, reject) => {
      const child = spawn(process.execPath, [
        path.resolve(directory, '..', 'acp-probe.mjs'),
        '--command', process.execPath,
        '--arg', path.join(directory, 'acp-fixture.mjs'),
        '--arg', 'proxy-network', '--arg', proxy.baseURL,
        '--timeout-ms', '4000', '--prompt', 'Reply only ACP_M0_OK',
        '--inflight-ipc', 'true',
      ], { stdio: ['ignore', 'pipe', 'pipe', 'ipc'], windowsHide: true })
      proxy.attachProbe(child)
      let stdout = ''
      child.stdout.on('data', chunk => { stdout += chunk })
      child.stderr.resume()
      child.once('error', reject)
      child.once('close', code => {
        try { resolve({ code, report: JSON.parse(stdout) }) } catch (error) { reject(error) }
      })
    })
    assert.equal(outcome.code, 0)
    assert.equal(received, 2)
    assert.equal(outcome.report.cancellation_evidence.agent_output_observed, false)
    assert.equal(outcome.report.cancellation_evidence.activity_type, 'upstream_http_request')
    assert.equal(outcome.report.cancellation_evidence.network_witness.connected, true)
    assert.equal(outcome.report.cancellation_evidence.network_witness.request_body_finished, true)
    assert.equal(outcome.report.cancellation_evidence.network_witness.response_complete_at_confirmation, false)
    assert.equal(outcome.report.cancellation_evidence.stop_reason, 'cancelled')
    await new Promise(resolve => setTimeout(resolve, 20))
    assert.equal(secondAborted, true)
    const witness = proxy.summary()[1]
    assert.equal(witness.client_disconnected, true)
    assert.equal(witness.upstream_aborted, true)
    assert.ok(!JSON.stringify(outcome.report).includes(key))
  } finally {
    await proxy.close()
    await service.close()
  }
})

test('proxy rejects missing credential and does not forward unauthorized traffic', async () => {
  let received = 0
  const service = await upstream((request, response) => { received++; request.resume(); response.end('{}') })
  const proxy = await startInflightProxy({ upstreamBaseURL: service.baseURL, authorizationHash })
  try {
    const result = await fetch(`${proxy.baseURL}/chat/completions`, { method: 'POST', body: '{}' })
    assert.equal(result.status, 403)
    assert.equal(received, 0)
    assert.deepEqual(proxy.summary(), [])
  } finally {
    await proxy.close()
    await service.close()
  }
})

test('confirmation refuses a request that completed after initial in-flight observation', async () => {
  const service = await upstream((request, response) => { request.resume(); request.on('end', () => response.writeHead(200).end('{}')) })
  const proxy = await startInflightProxy({ upstreamBaseURL: service.baseURL, authorizationHash })
  const messages = []
  const child = new EventEmitter()
  child.connected = true
  child.send = (message, callback) => { messages.push(message); callback?.() }
  proxy.attachProbe(child)
  const call = () => fetch(`${proxy.baseURL}/chat/completions`, { method: 'POST', headers: { authorization: `Bearer ${key}` }, body: '{}' }).then(response => response.text())
  try {
    await call()
    child.emit('message', { type: 'arm_inflight', probe_id: 'stale-probe' })
    assert.equal(messages.at(-1).ready, true)
    await call()
    assert.ok(messages.some(message => message.type === 'upstream_inflight'))
    child.emit('message', { type: 'confirm_inflight', probe_id: 'stale-probe' })
    assert.equal(messages.at(-1).type, 'inflight_confirmed')
    assert.equal(messages.at(-1).inflight, false)
    assert.equal(messages.at(-1).response_complete, true)
  } finally {
    await proxy.close()
    await service.close()
  }
})
