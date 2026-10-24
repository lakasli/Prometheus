// These tests exercise false-positive resistance using a protocol fixture.
// They do not constitute real Harness/model validation.
import test from 'node:test'
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const directory = path.dirname(fileURLToPath(import.meta.url))
const probe = path.resolve(directory, '..', 'acp-probe.mjs')
const fixture = path.join(directory, 'acp-fixture.mjs')
async function run(mode, withPrompt = true) {
  return await new Promise((resolve, reject) => {
    const args = [probe, '--command', process.execPath, '--arg', fixture, '--arg', mode, '--timeout-ms', '3000']
    if (withPrompt) args.push('--prompt', 'Reply only ACP_M0_OK')
    const child = spawn(process.execPath, args, { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true })
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', chunk => { stdout += chunk })
    child.stderr.on('data', chunk => { stderr += chunk })
    child.once('error', reject)
    child.once('close', code => {
      try { resolve({ code, report: JSON.parse(stdout), stderr }) } catch (error) { reject(new Error(`fixture probe did not return JSON: ${error.message}`)) }
    })
  })
}

test('requires matching assistant output and cancels only after agent activity', async () => {
  const { code, report } = await run('valid')
  assert.equal(code, 0)
  assert.equal(report.model_evidence.expected_marker_received, true)
  assert.equal(report.model_evidence.assistant_message_chunks, 2)
  assert.equal(report.cancellation_evidence.activity_type, 'agent_thought_chunk')
  assert.equal(report.cancellation_evidence.stop_reason, 'cancelled')
  assert.equal(report.cancellation_evidence.cancel_sent_before_prompt_settlement, true)
  assert.equal(report.status, 'passed_with_model')
  assert.ok(!JSON.stringify(report).includes('fixture activity'))
})

test('user echo and output from another session cannot satisfy model reply', async () => {
  const { code, report } = await run('no-assistant')
  assert.equal(code, 1)
  assert.equal(report.model_evidence.expected_marker_received, false)
  assert.equal(report.checks.find(item => item.name === 'real_model_prompt').status, 'failed')
})

test('completed generation cannot be reported as verified in-flight cancellation', async () => {
  const { code, report } = await run('already-finished')
  assert.equal(code, 1)
  assert.equal(report.cancellation_evidence.stop_reason, 'end_turn')
  assert.equal(report.checks.find(item => item.name === 'cancel_inflight_model_prompt').status, 'failed')
})

test('queue acceptance/user events alone cannot establish cancellable activity', async () => {
  const { code, report } = await run('no-cancellable-activity')
  assert.equal(code, 1)
  assert.equal(report.cancellation_evidence.agent_output_observed, false)
  assert.equal(report.cancellation_evidence.cancel_sent_before_prompt_settlement, false)
})

test('no-prompt flow still reports model checks as not run', async () => {
  const { code, report } = await run('valid', false)
  assert.equal(code, 0)
  assert.equal(report.status, 'passed_without_model')
  assert.equal(report.checks.find(item => item.name === 'real_model_prompt').status, 'not_run')
  assert.equal(report.checks.find(item => item.name === 'cancel_inflight_model_prompt').status, 'not_run')
  assert.equal(report.model_evidence, undefined)
})
