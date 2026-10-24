#!/usr/bin/env node
// Run against a real Harness process. This file is not an ACP mock server.
import { spawn } from 'node:child_process'
import { mkdir, mkdtemp, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import readline from 'node:readline'

const argv = process.argv.slice(2)
const options = { command: 'dsh', args: [], timeout: 30000, prompt: null }
for (let index = 0; index < argv.length; index++) {
  const name = argv[index]
  if (name === '--help') {
    console.log('node tools/m0/acp-probe.mjs --command <executable> [--arg <argument>] [--cwd <workspace>] [--home <isolated DSH_HOME>] [--report <file>] [--prompt <harmless prompt>] [--timeout-ms <milliseconds>]')
    console.log('Pass every argument separately, including: --arg --profile --arg acp. No prompt means no model invocation.')
    process.exit(0)
  }
  const value = argv[++index]
  if (value === undefined) throw new Error(`missing value for ${name}`)
  if (name === '--command') options.command = value
  else if (name === '--arg') options.args.push(value)
  else if (name === '--cwd') options.cwd = path.resolve(value)
  else if (name === '--home') options.home = path.resolve(value)
  else if (name === '--report') options.report = path.resolve(value)
  else if (name === '--prompt') options.prompt = value
  else if (name === '--timeout-ms') options.timeout = Number(value)
  else throw new Error(`unknown option: ${name}`)
}
if (!Number.isSafeInteger(options.timeout) || options.timeout < 1000) throw new Error('invalid timeout')
if (!options.args.length) options.args = ['--profile', 'acp']
const scratch = await mkdtemp(path.join(tmpdir(), 'dsh-acp-probe-'))
options.cwd ??= path.join(scratch, 'workspace')
options.home ??= path.join(scratch, 'harness-home')
await mkdir(options.cwd, { recursive: true })
await mkdir(options.home, { recursive: true })
const report = {
  schema_version: 1,
  started_at: new Date().toISOString(),
  implementation: 'real-harness-stdio',
  checks: [],
  model_prompt_requested: Boolean(options.prompt),
  credentials_in_report: false,
}

function safeError(error) {
  let message = String(error.message)
  for (const [name, value] of Object.entries(process.env)) {
    if (/KEY|TOKEN|PASSWORD|SECRET/i.test(name) && value && value.length > 4) message = message.split(value).join('[REDACTED]')
  }
  return message.replace(/(Bearer\s+)\S+/gi, '$1[REDACTED]').slice(0, 400)
}

function connect() {
  const child = spawn(options.command, options.args, {
    cwd: options.cwd,
    env: { ...process.env, DSH_HOME: options.home },
    stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true,
    shell: false,
  })
  const waiting = new Map()
  let sequence = 0
  let closed = false
  let outputBytes = 0
  let stderrBytes = 0
  const updates = []
  const rejectAll = (error) => {
    for (const pending of waiting.values()) { clearTimeout(pending.timer); pending.reject(error) }
    waiting.clear()
  }
  child.once('error', error => rejectAll(error))
  child.once('exit', (code, signal) => {
    closed = true
    rejectAll(new Error(`Harness exited: code=${code}, signal=${signal}`))
  })
  child.stderr.on('data', data => { stderrBytes += data.length })
  const lines = readline.createInterface({ input: child.stdout, crlfDelay: Infinity })
  lines.on('line', line => {
    outputBytes += Buffer.byteLength(line)
    if (line.length > 4 * 1024 * 1024 || outputBytes > 16 * 1024 * 1024) {
      rejectAll(new Error('protocol output limit exceeded'))
      child.kill()
      return
    }
    let message
    try { message = JSON.parse(line) } catch {
      rejectAll(new Error('Harness stdout contains a non-JSON protocol line'))
      child.kill()
      return
    }
    if ('id' in message && !message.method) {
      const pending = waiting.get(message.id)
      if (!pending) return
      waiting.delete(message.id)
      clearTimeout(pending.timer)
      if (message.error) pending.reject(new Error(`ACP error ${message.error.code}: ${String(message.error.message).slice(0, 300)}`))
      else pending.resolve(message.result)
    } else if (message.method && 'id' in message) {
      // A probe never grants code execution or filesystem access.
      if (message.method === 'session/request_permission') {
        child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: message.id, result: { outcome: { outcome: 'cancelled' } } }) + '\n')
      } else {
        child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'probe does not expose client tools' } }) + '\n')
      }
    } else if (message.method === 'session/update') {
      updates.push({ session_id: message.params?.sessionId, type: message.params?.update?.sessionUpdate })
    }
  })
  return {
    request(method, params = {}) {
      return new Promise((resolve, reject) => {
        if (closed) return reject(new Error('Harness connection already closed'))
        const id = ++sequence
        const timer = setTimeout(() => { waiting.delete(id); reject(new Error(`ACP timeout: ${method}`)) }, options.timeout)
        waiting.set(id, { resolve, reject, timer })
        child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n')
      })
    },
    notify(method, params) {
      child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method, params }) + '\n')
    },
    async stop() {
      child.stdin.end()
      await Promise.race([
        new Promise(resolve => { if (closed) resolve(); else child.once('exit', resolve) }),
        new Promise(resolve => setTimeout(resolve, 3000)),
      ])
      if (!closed) child.kill()
      lines.close()
      rejectAll(new Error('probe connection closed'))
      return { stdout_bytes: outputBytes, stderr_bytes: stderrBytes }
    },
    updates,
  }
}

async function check(name, operation) {
  try {
    const result = await operation()
    report.checks.push({ name, status: 'passed' })
    return result
  } catch (error) {
    report.checks.push({ name, status: 'failed', error: safeError(error) })
    throw error
  }
}

async function initialize(connection) {
  const result = await connection.request('initialize', {
    protocolVersion: 1,
    clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false },
    clientInfo: { name: 'dsh-m0-validation', version: '0.1.0' },
  })
  if (result.protocolVersion !== 1) throw new Error('ACP v1 was not negotiated')
  report.agent = result.agentInfo
  report.capabilities = result.agentCapabilities
  return result
}

let connection = connect()
try {
  await check('initialize_real_process', () => initialize(connection))
  const session = await check('create_persistent_session', () => connection.request('session/new', { cwd: options.cwd, mcpServers: [] }))
  if (!session.sessionId) throw new Error('session/new returned no sessionId')
  if (options.prompt) {
    await check('real_model_prompt', () => connection.request('session/prompt', { sessionId: session.sessionId, prompt: [{ type: 'text', text: options.prompt }] }))
    // Cancel a second prompt while it is in flight. This verifies the server's
    // cancellation path, not only the no-op idle cancellation notification.
    await check('cancel_inflight_model_prompt', async () => {
      const pending = connection.request('session/prompt', { sessionId: session.sessionId, prompt: [{ type: 'text', text: options.prompt }] })
      connection.notify('session/cancel', { sessionId: session.sessionId })
      const result = await pending
      if (result.stopReason !== 'cancelled') throw new Error(`expected cancelled, got ${result.stopReason}`)
    })
  } else {
    report.checks.push({ name: 'real_model_prompt', status: 'not_run', reason: 'no --prompt supplied; no model request' })
    report.checks.push({ name: 'cancel_inflight_model_prompt', status: 'not_run', reason: 'requires actual in-flight model work' })
    connection.notify('session/cancel', { sessionId: session.sessionId })
  }
  await check('close_session', () => connection.request('session/close', { sessionId: session.sessionId }))
  report.first_process = await connection.stop()
  connection = connect()
  await check('initialize_restarted_process', () => initialize(connection))
  const listed = await check('list_after_process_restart', () => connection.request('session/list', { cwd: options.cwd }))
  if (!(listed.sessions ?? []).some(item => item.sessionId === session.sessionId)) throw new Error('persisted session missing from session/list')
  await check('resume_after_process_restart', () => connection.request('session/resume', { sessionId: session.sessionId, cwd: options.cwd, mcpServers: [] }))
  await check('close_resumed_session', () => connection.request('session/close', { sessionId: session.sessionId }))
} catch {
  report.status = 'failed'
  process.exitCode = 1
} finally {
  report.final_process = await connection.stop()
  report.status ??= options.prompt ? 'passed_with_model' : 'passed_without_model'
  report.finished_at = new Date().toISOString()
  if (options.report) {
    await mkdir(path.dirname(options.report), { recursive: true })
    await writeFile(options.report, JSON.stringify(report, null, 2) + '\n')
  }
  console.log(JSON.stringify(report, null, 2))
}
