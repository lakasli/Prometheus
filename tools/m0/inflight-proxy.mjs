// M0-only loopback proxy. It never records headers, request/response bodies, or
// credentials. Network lifecycle metadata witnesses a real in-flight request.
import http from 'node:http'
import https from 'node:https'
import { createHash, randomBytes, timingSafeEqual } from 'node:crypto'

export async function startInflightProxy({ upstreamBaseURL, authorizationHash, timeoutMs = 90000 }) {
  const upstreamURL = new URL(`${upstreamBaseURL.replace(/\/+$/, '')}/chat/completions`)
  if (!['http:', 'https:'].includes(upstreamURL.protocol) || upstreamURL.username || upstreamURL.password || upstreamURL.search || upstreamURL.hash) throw new Error('unsupported upstream model endpoint')
  if (!Buffer.isBuffer(authorizationHash) || authorizationHash.length !== 32) throw new Error('a SHA-256 authorization hash is required')
  const prefix = `/m0-${randomBytes(24).toString('hex')}`
  const requests = new Map()
  let count = 0
  let armed = null
  let child = null
  let disposed = false
  const sockets = new Set()
  const send = message => {
    if (child?.connected) child.send(message, () => {})
  }
  const isInflight = entry => Boolean(entry?.connected && entry.request_body_finished && !entry.response_complete && !entry.closed)
  const metadata = entry => ({
    request_index: entry.index,
    connected: entry.connected,
    request_body_finished: entry.request_body_finished,
    response_headers_received: entry.response_headers_received,
    response_complete: entry.response_complete,
    closed: entry.closed,
  })
  const maybeWitness = entry => {
    if (!armed || armed.request_index !== entry.index || armed.announced || !isInflight(entry)) return
    armed.announced = true
    send({ type: 'upstream_inflight', probe_id: armed.probe_id, ...metadata(entry) })
  }
  const server = http.createServer((incoming, outgoing) => {
    // The random path is private to the child profile and only the configured
    // credential may be forwarded. This endpoint is never a general proxy.
    const hash = createHash('sha256').update(incoming.headers.authorization ?? '').digest()
    if (disposed || incoming.method !== 'POST' || incoming.url !== `${prefix}/chat/completions` || !timingSafeEqual(hash, authorizationHash)) {
      outgoing.writeHead(403).end()
      incoming.resume()
      return
    }
    const contentLength = incoming.headers['content-length']
    if (contentLength && (!/^\d+$/.test(contentLength) || Number(contentLength) > 1024 * 1024)) {
      outgoing.writeHead(413).end()
      incoming.resume()
      return
    }
    const entry = {
      index: ++count,
      connected: false,
      request_body_finished: false,
      response_headers_received: false,
      response_complete: false,
      closed: false,
      client_disconnected: false,
      upstream_aborted: false,
    }
    requests.set(entry.index, entry)
    const transport = upstreamURL.protocol === 'https:' ? https : http
    const forwarded = transport.request(upstreamURL, {
      method: 'POST',
      headers: {
        'content-type': incoming.headers['content-type'] ?? 'application/json',
        accept: incoming.headers.accept ?? 'application/json',
        authorization: incoming.headers.authorization,
        ...(contentLength ? { 'content-length': contentLength } : {}),
      },
      timeout: timeoutMs,
      agent: false,
    })
    entry.abort = () => { entry.upstream_aborted = true; forwarded.destroy() }
    forwarded.on('socket', socket => {
      const connected = () => { entry.connected = true; maybeWitness(entry) }
      // HTTPS secureConnect means TLS is established, not only a local socket
      // allocation; HTTP connect similarly witnesses the real TCP connection.
      if (upstreamURL.protocol === 'https:') socket.once('secureConnect', connected)
      else if (socket.connecting) socket.once('connect', connected)
      else connected()
    })
    forwarded.on('finish', () => {
      // This is ClientRequest.finish: the outgoing request has been handed to
      // the established connection. It is not merely an ACP queue event.
      entry.request_body_finished = true
      maybeWitness(entry)
    })
    forwarded.on('response', response => {
      entry.response_headers_received = true
      if (outgoing.destroyed) { response.destroy(); return }
      outgoing.writeHead(response.statusCode ?? 502, {
        'content-type': response.headers['content-type'] ?? 'application/json',
        'cache-control': 'no-store',
        ...(response.headers['content-encoding'] ? { 'content-encoding': response.headers['content-encoding'] } : {}),
      })
      response.on('end', () => {
        entry.response_complete = true
        if (armed?.request_index === entry.index) send({ type: 'upstream_completed', probe_id: armed.probe_id, ...metadata(entry) })
      })
      response.on('error', () => outgoing.destroy())
      response.pipe(outgoing)
    })
    forwarded.on('timeout', () => forwarded.destroy())
    forwarded.on('error', () => {
      if (!outgoing.headersSent) outgoing.writeHead(502).end()
      else outgoing.destroy()
    })
    forwarded.on('close', () => { entry.closed = true })
    let bodyBytes = 0
    incoming.on('data', chunk => {
      bodyBytes += chunk.length
      if (bodyBytes > 1024 * 1024) {
        forwarded.destroy()
        incoming.destroy()
      }
    })
    incoming.on('error', () => forwarded.destroy())
    outgoing.on('close', () => {
      if (!entry.response_complete) {
        entry.client_disconnected = true
        entry.abort()
      }
    })
    incoming.pipe(forwarded)
  })
  server.on('connection', socket => {
    sockets.add(socket)
    socket.once('close', () => sockets.delete(socket))
  })
  server.requestTimeout = timeoutMs
  server.headersTimeout = Math.min(timeoutMs, 15000)
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  const baseURL = `http://127.0.0.1:${server.address().port}${prefix}`
  return {
    baseURL,
    attachProbe(probe) {
      if (child) throw new Error('only one ACP probe may use this proxy')
      child = probe
      child.on('message', message => {
        if (message?.type === 'arm_inflight' && typeof message.probe_id === 'string' && message.probe_id.length <= 80) {
          const active = [...requests.values()].some(entry => isInflight(entry))
          if (active || count < 1) {
            send({ type: 'inflight_armed', probe_id: message.probe_id, ready: false })
            return
          }
          armed = { probe_id: message.probe_id, request_index: count + 1, announced: false }
          send({ type: 'inflight_armed', probe_id: armed.probe_id, request_index: armed.request_index, ready: true })
        } else if (message?.type === 'confirm_inflight' && armed?.probe_id === message.probe_id) {
          const entry = requests.get(armed.request_index)
          send({
            type: 'inflight_confirmed',
            probe_id: armed.probe_id,
            inflight: isInflight(entry),
            ...(entry ? metadata(entry) : { request_index: armed.request_index }),
          })
        }
      })
    },
    summary() {
      return [...requests.values()].map(entry => ({ ...metadata(entry), client_disconnected: entry.client_disconnected, upstream_aborted: entry.upstream_aborted }))
    },
    async close() {
      disposed = true
      for (const entry of requests.values()) if (!entry.response_complete && !entry.closed) entry.abort()
      for (const socket of sockets) socket.destroy()
      await new Promise(resolve => server.close(resolve))
    },
  }
}
