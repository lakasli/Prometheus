// Protocol fixture for testing probe assertions only. Never use it as M0 evidence.
import readline from 'node:readline'
const mode = process.argv[2] ?? 'valid'
const sessionId = 'unit-test-session'
let prompts = 0
let active = null
let emittedAgent = false
let controller = null
const send = message => process.stdout.write(JSON.stringify({ jsonrpc: '2.0', ...message }) + '\n')
const result = (id, value) => send({ id, result: value })
const update = (type, text, id = sessionId) => send({ method: 'session/update', params: { sessionId: id, update: { sessionUpdate: type, content: { type: 'text', text } } } })
const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity })
lines.on('line', line => {
  const message = JSON.parse(line)
  switch (message.method) {
    case 'initialize': result(message.id, { protocolVersion: 1, agentInfo: { name: 'unit-test-fixture' }, agentCapabilities: {} }); break
    case 'session/new': result(message.id, { sessionId }); break
    case 'session/list': result(message.id, { sessions: [{ sessionId }] }); break
    case 'session/resume': result(message.id, {}); break
    case 'session/close': result(message.id, {}); break
    case 'session/prompt':
      prompts++
      if (mode === 'proxy-network') {
        active = message.id
        controller = new AbortController()
        const ownId = message.id
        void fetch(`${process.argv[3]}/chat/completions`, {
          method: 'POST',
          headers: { 'content-type': 'application/json', authorization: 'Bearer unit-test-model-key' },
          body: JSON.stringify({ messages: [], stream: false }),
          signal: controller.signal,
        }).then(response => response.json()).then(response => {
          if (active === ownId) {
            update('agent_message_chunk', response.text)
            result(ownId, { stopReason: 'end_turn' })
            active = null
          }
        }).catch(() => {
          if (active === ownId) {
            send({ id: ownId, error: { code: -32603, message: 'fixture upstream failed' } })
            active = null
          }
        })
        break
      }
      if (prompts === 1) {
        update('user_message_chunk', 'ACP_M0_OK')
        update('agent_message_chunk', 'ACP_M0_OK', 'another-session')
        if (mode !== 'no-assistant') {
          update('agent_message_chunk', 'ACP_')
          update('agent_message_chunk', 'M0_OK')
        }
        result(message.id, { stopReason: 'end_turn' })
      } else {
        active = message.id
        emittedAgent = false
        update('user_message_chunk', 'user content is not agent activity')
        if (mode === 'already-finished') {
          update('agent_message_chunk', 'A completed response')
          result(active, { stopReason: 'end_turn' })
          active = null
        } else if (mode === 'no-cancellable-activity') {
          result(active, { stopReason: 'end_turn' })
          active = null
        } else {
          setTimeout(() => {
            if (active !== null) {
              emittedAgent = true
              update('agent_thought_chunk', 'fixture activity: not a real model thought')
            }
          }, 20)
        }
      }
      break
    case 'session/cancel':
      if (active !== null) {
        if (mode === 'proxy-network') {
          controller.abort()
          result(active, { stopReason: 'cancelled' })
          active = null
          break
        }
        result(active, { stopReason: emittedAgent ? 'cancelled' : 'end_turn' })
        active = null
      }
      break
    default:
      if ('id' in message) send({ id: message.id, error: { code: -32601, message: 'fixture method not implemented' } })
  }
})
