#!/usr/bin/env node
/**
 * Read-only M1 QQ HTTP diagnostics. Run with Node.js 24+ on the target host.
 * It does not open a gateway WebSocket or alter the database/configuration.
 * Only fixed classifications, HTTP statuses, numeric platform codes and a
 * redacted gateway hostname are printed. Credentials and response bodies stay
 * in process memory. Production credentials are never accepted as CLI args.
 */
import { DatabaseSync } from 'node:sqlite';
import { lstatSync, readFileSync, realpathSync } from 'node:fs';
import { resolve, join, relative, isAbsolute } from 'node:path';
import { pathToFileURL } from 'node:url';

const TOKEN_URL = 'https://bots.qq.com/app/getAppAccessToken';
const GATEWAY_URL = 'https://api.sgroup.qq.com/gateway';
const MAX_RESPONSE_BYTES = 65536;

export class ProbeError extends Error {
  constructor(code) { super(code); this.code = code; }
}

export function numericCode(body) {
  for (const key of ['code', 'err_code', 'retcode']) {
    const value = body?.[key];
    if (typeof value === 'number' && Number.isSafeInteger(value)) return value;
    if (typeof value === 'string' && /^-?\d{1,12}$/.test(value)) return Number(value);
  }
  return null;
}

export function gatewayMetadata(raw, confidential = []) {
  if (typeof raw !== 'string' || raw.length > 8192) return { present: false, host: null, allowed: false };
  try {
    const url = new URL(raw);
    const host = url.hostname.toLowerCase();
    const allowed = url.protocol === 'wss:' && !url.username && !url.password
      && (host === 'api.bot.qq.com' || host.endsWith('.bot.qq.com') || host.endsWith('.sgroup.qq.com'));
    let displayHost = host;
    for (const value of confidential) if (value) displayHost = displayHost.split(String(value).toLowerCase()).join('[redacted]');
    return { present: true, host: displayHost, allowed };
  } catch {
    return { present: true, host: null, allowed: false };
  }
}

export async function boundedJson(response) {
  if (Number(response.headers.get('content-length')) > MAX_RESPONSE_BYTES) {
    await response.body?.cancel();
    throw new ProbeError('response_too_large');
  }
  let length = 0;
  const chunks = [];
  if (!response.body) throw new ProbeError('empty_response');
  const reader = response.body.getReader();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.length;
      if (length > MAX_RESPONSE_BYTES) {
        await reader.cancel();
        throw new ProbeError('response_too_large');
      }
      chunks.push(value);
    }
  } catch (error) {
    if (error instanceof ProbeError) throw error;
    throw new ProbeError('response_read_failed');
  } finally {
    reader.releaseLock();
  }
  try { return JSON.parse(Buffer.concat(chunks, length).toString('utf8')); }
  catch { throw new ProbeError('response_not_json'); }
}

function credentials(dataDir) {
  const root = realpathSync(dataDir);
  const dbFile = join(root, 'dsh.db');
  const db = new DatabaseSync(dbFile, { readOnly: true });
  try {
    const row = db.prepare("SELECT c.config_json FROM config_revisions c JOIN settings s ON s.key='active_revision' AND c.revision=CAST(s.value AS INTEGER)").get();
    if (!row) throw new ProbeError('active_configuration_missing');
    const config = JSON.parse(row.config_json);
    const qq = config.qq;
    if (!qq || typeof qq.app_id !== 'string' || !/^\d{1,32}$/.test(qq.app_id)) throw new ProbeError('qq_app_configuration_invalid');
    if (typeof qq.credential_ref !== 'string' || !/^[a-f0-9-]{36}$/i.test(qq.credential_ref)) throw new ProbeError('qq_credential_reference_invalid');
    const stored = db.prepare("SELECT kind FROM secrets WHERE id=?").get(qq.credential_ref);
    if (stored?.kind !== 'qq_app_secret') throw new ProbeError('qq_credential_type_invalid');
    const file = join(root, 'secrets', qq.credential_ref);
    const stat = lstatSync(file);
    if (!stat.isFile() || stat.isSymbolicLink()) throw new ProbeError('secret_file_not_regular');
    if (process.platform !== 'win32' && (stat.mode & 0o077) !== 0) throw new ProbeError('secret_file_permissions_not_restricted');
    const actual = realpathSync(file);
    const child = relative(join(root, 'secrets'), actual);
    if (child.startsWith('..') || isAbsolute(child)) throw new ProbeError('secret_file_outside_store');
    const secret = readFileSync(file, 'utf8');
    if (!secret || secret.length > 8192) throw new ProbeError('secret_file_size_invalid');
    return { appId: qq.app_id, secret, enabled: qq.enabled === true, transport: qq.transport ?? 'websocket' };
  } finally { db.close(); }
}

async function fetchJson(url, init, emit, stage) {
  let response;
  try {
    response = await fetch(url, { ...init, redirect: 'manual', signal: AbortSignal.timeout(12000) });
  } catch (error) {
    emit({ stage, result: 'network_failed', kind: ['TimeoutError', 'AbortError'].includes(error?.name) ? 'timeout' : 'connection_or_tls' });
    throw new ProbeError(`${stage}_network_failed`);
  }
  let body;
  try { body = await boundedJson(response); }
  catch (error) {
    emit({ stage, http_status: response.status, result: 'invalid_response', kind: error instanceof ProbeError ? error.code : 'response_failed' });
    throw new ProbeError(`${stage}_invalid_response`);
  }
  emit({ stage, http_status: response.status, business_code: numericCode(body), result: response.ok ? 'http_success' : 'http_error' });
  if (!response.ok) throw new ProbeError(`${stage}_http_error`);
  return body;
}

export async function runProbe(dataDir, emit = value => console.log(JSON.stringify(value))) {
  const config = credentials(dataDir);
  emit({ stage: 'configuration', enabled: config.enabled, transport: ['websocket', 'webhook'].includes(config.transport) ? config.transport : 'unknown', credential_present: true, database_mode: 'read_only', websocket_opened: false });
  const tokenBody = await fetchJson(TOKEN_URL, {
    method: 'POST', headers: { 'Content-Type': 'application/json', 'User-Agent': 'DSH-QQ-HTTP-Probe/1' },
    body: JSON.stringify({ appId: config.appId, clientSecret: config.secret }),
  }, emit, 'token');
  const token = tokenBody.access_token;
  if (typeof token !== 'string' || !token || token.length > 16384) {
    emit({ stage: 'token', token_present: false });
    throw new ProbeError('token_missing');
  }
  emit({ stage: 'token', token_present: true });
  const gateway = await fetchJson(GATEWAY_URL, {
    method: 'GET', headers: { Authorization: `QQBot ${token}`, 'User-Agent': 'DSH-QQ-HTTP-Probe/1' },
  }, emit, 'gateway');
  const metadata = gatewayMetadata(gateway.url, [config.appId, config.secret, token]);
  emit({ stage: 'gateway_address', ...metadata });
  if (!metadata.present || !metadata.allowed) throw new ProbeError('gateway_address_unusable');
  emit({ stage: 'completed', http_configuration_usable: true, websocket_tested: false, messages_tested: false });
}

async function main() {
  const args = process.argv.slice(2);
  if (args.length !== 2 || args[0] !== '--data-dir') {
    console.error('Usage: node tools/qq-probe.mjs --data-dir /path/to/dsh/data');
    process.exitCode = 2; return;
  }
  try { await runProbe(resolve(args[1])); }
  catch (error) {
    const code = error instanceof ProbeError ? error.code : ['ENOENT', 'EACCES', 'EPERM'].includes(error?.code) ? error.code : 'configuration_or_probe_failed';
    console.log(JSON.stringify({ stage: 'failed', code }));
    process.exitCode = 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) await main();
