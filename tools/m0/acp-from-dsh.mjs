#!/usr/bin/env node
// Optional real-provider validation: reads M1's credential reference locally,
// never exposes a key as a CLI argument or changes the production database.
import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { readFile, writeFile, mkdtemp, chmod, stat } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { startInflightProxy } from './inflight-proxy.mjs'

const args = process.argv.slice(2)
if (args.includes('--help')) {
  console.log('node tools/m0/acp-from-dsh.mjs --data-dir <M1 data directory> --harness-bin <absolute dsh/lib/bin.js> --report <result.json>')
  console.log('Reads the active M1 model and its owner-only credential locally. Sends a harmless real prompt and a cancelled prompt; no keys in CLI/output.')
  process.exit(0)
}
const options = {}
for (let i = 0; i < args.length; i += 2) {
  if (!['--data-dir', '--harness-bin', '--report'].includes(args[i]) || !args[i + 1]) throw new Error('invalid probe options; see --help')
  options[args[i].slice(2)] = path.resolve(args[i + 1])
}
if (!options['data-dir'] || !options['harness-bin'] || !options.report) throw new Error('--data-dir, --harness-bin and --report are required')
const { DatabaseSync } = await import('node:sqlite')
const db = new DatabaseSync(path.join(options['data-dir'], 'dsh.db'), { readOnly: true })
let model
let key
try {
  const row = db.prepare("SELECT c.config_json FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").get()
  if (!row) throw new Error('no active M1 configuration')
  model = JSON.parse(row.config_json).model
  if (!model || !/^[0-9a-f-]{36}$/i.test(model.credential_ref ?? '')) throw new Error('active model has no valid credential reference')
  if (!db.prepare("SELECT 1 FROM secrets WHERE id=? AND kind='model_api_key'").get(model.credential_ref)) throw new Error('model credential metadata is missing')
  const secretPath = path.join(options['data-dir'], 'secrets', model.credential_ref)
  const metadata = await stat(secretPath)
  if (process.platform !== 'win32' && (metadata.mode & 0o077)) throw new Error('model credential file must be owner-only')
  key = await readFile(secretPath, 'utf8')
  if (!key || key.includes('\n') || key.includes('\r')) throw new Error('invalid credential file')
} finally {
  db.close()
}
const base = new URL(model.base_url)
if (!['https:', 'http:'].includes(base.protocol) || base.username || base.password || base.search || base.hash) throw new Error('unsupported active model endpoint')
const scratch = await mkdtemp(path.join(tmpdir(), 'dsh-acp-live-'))
await chmod(scratch, 0o700)
const proxy = await startInflightProxy({
  upstreamBaseURL: model.base_url,
  authorizationHash: createHash('sha256').update(`Bearer ${key}`).digest(),
})
const patchPath = path.join(scratch, 'model.patch.json')
const patch = [
  {
    id: 'llm-pi-ai',
    config: {
      providers: {
        'dsh-m0-provider': {
          apiKeyEnv: 'DSH_M0_MODEL_KEY',
          api: 'openai-completions',
          baseURL: proxy.baseURL,
          // The loopback URL must not change endpoint heuristics into OpenAI
          // defaults. Keep the same generic Chat Completions wire contract as M1.
          compat: { supportsStore: false, supportsDeveloperRole: false, supportsUsageInStreaming: false, maxTokensField: 'max_tokens' },
          models: [{ id: model.model, name: model.model, contextWindow: 65536, maxTokens: 1024, input: ['text'] }],
          retryPolicy: { mode: 'normal', maxRetries: 0 },
          timeoutMs: 60000,
        },
      },
    },
  },
  { id: 'acp', config: { provider: 'dsh-m0-provider', model: model.model } },
  { id: 'agent-default-model', config: { provider: 'dsh-m0-provider', model: model.model } },
]
await writeFile(patchPath, JSON.stringify(patch, null, 2), { mode: 0o600 })
const toolDir = path.dirname(fileURLToPath(import.meta.url))
const env = {}
for (const name of ['PATH', 'HOME', 'USERPROFILE', 'SYSTEMROOT', 'SystemRoot', 'WINDIR', 'COMSPEC', 'PATHEXT', 'TEMP', 'TMP', 'LANG', 'LC_ALL', 'SSL_CERT_FILE', 'SSL_CERT_DIR', 'NODE_EXTRA_CA_CERTS', 'HTTP_PROXY', 'HTTPS_PROXY', 'NO_PROXY']) {
  if (process.env[name]) env[name] = process.env[name]
}
env.DSH_M0_MODEL_KEY = key
key = undefined
const child = spawn(process.execPath, [
  path.join(toolDir, 'acp-probe.mjs'),
  '--command', process.execPath,
  '--arg', options['harness-bin'],
  '--arg', '--profile', '--arg', 'acp',
  '--arg', '--patch', '--arg', path.join(toolDir, 'no-tools.patch.json'),
  '--arg', '--patch', '--arg', patchPath,
  '--home', path.join(scratch, 'home'),
  '--cwd', path.join(scratch, 'workspace'),
  '--report', options.report,
  '--timeout-ms', '90000',
  '--inflight-ipc', 'true',
  '--prompt', 'Do not use any tools. Reply with exactly ACP_M0_OK.',
], { env, stdio: ['inherit', 'inherit', 'inherit', 'ipc'], shell: false, windowsHide: true })
proxy.attachProbe(child)
let finishing = false
async function finish(code) {
  if (finishing) return
  finishing = true
  // Observe cancellation disconnect before cleanup itself aborts any remaining
  // sockets, so cleanup is not misrepresented as proof of ACP cancellation.
  await new Promise(resolve => setTimeout(resolve, 50))
  const proxyEvidence = proxy.summary()
  await proxy.close()
  try {
    const result = JSON.parse(await readFile(options.report, 'utf8'))
    result.proxy_evidence = { listen_scope: 'loopback_only', stores_headers_or_bodies: false, upstream_requests: proxyEvidence }
    await writeFile(options.report, JSON.stringify(result, null, 2) + '\n', { mode: 0o600 })
  } catch {
    console.error('Unable to append non-secret network lifecycle evidence')
    code = 1
  }
  process.exitCode = code ?? 1
}
child.once('error', () => { console.error('Unable to launch real ACP probe'); void finish(1) })
child.once('exit', code => { void finish(code) })
