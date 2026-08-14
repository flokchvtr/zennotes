// Tiny local render process for the Tauri build: the Go server proxies
// POST /tikz here (ZENNOTES_TIKZ_UPSTREAM). Reuses the desktop's tikz module
// (tikz-core.mjs, esbuild-compiled from apps/desktop/src/main/tikz.ts), which
// owns the wasm TeX engine, caching, and render serialization.
import { createServer } from 'node:http'
import { renderTikz } from './tikz-core.mjs'

const port = Number(process.env.TIKZ_PORT)
if (!Number.isInteger(port) || port <= 0) {
  console.error('TIKZ_PORT env var is required')
  process.exit(1)
}

const server = createServer((req, res) => {
  if (req.method !== 'POST' || req.url !== '/render') {
    res.writeHead(404).end()
    return
  }
  let body = ''
  req.on('data', (chunk) => (body += chunk))
  req.on('end', async () => {
    let result
    try {
      const { source } = JSON.parse(body)
      result = await renderTikz(String(source ?? ''))
    } catch (err) {
      result = { ok: false, error: err instanceof Error ? err.message : String(err) }
    }
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end(JSON.stringify(result))
  })
})

server.listen(port, '127.0.0.1', () => {
  console.log(`tikz render process on 127.0.0.1:${port}`)
})
