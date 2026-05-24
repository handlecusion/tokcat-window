import express from 'express'
import { createServer as createViteServer } from 'vite'
import http from 'node:http'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const PORT = 4061
const REFRESH_MS = 3 * 60 * 1000

function emptyGraph(year) {
  const y = year || String(new Date().getFullYear())
  const now = new Date().toISOString()
  return {
    meta: {
      generatedAt: now,
      version: 'tokcat-dev',
      dateRange: { start: '', end: '' },
    },
    summary: {
      totalTokens: 0,
      totalCost: 0,
      totalDays: 0,
      activeDays: 0,
      averagePerDay: 0,
      maxCostInSingleDay: 0,
      clients: [],
      models: [],
    },
    years: [
      {
        year: y,
        totalTokens: 0,
        totalCost: 0,
        range: { start: '', end: '' },
      },
    ],
    contributions: [],
  }
}

const cache = new Map()

function ensureEntry(year) {
  let entry = cache.get(year)
  if (!entry) {
    entry = { data: emptyGraph(year), lastFetched: Date.now(), subscribers: new Set(), timer: null }
    cache.set(year, entry)
  }
  return entry
}

function broadcast(year) {
  const entry = ensureEntry(year)
  const payload = JSON.stringify({
    year,
    fetchedAt: new Date(entry.lastFetched).toISOString(),
    payload: entry.data,
  })
  const msg = `event: data\ndata: ${payload}\n\n`
  for (const res of entry.subscribers) {
    try { res.write(msg) } catch {}
  }
}

function startTimer(year) {
  const entry = ensureEntry(year)
  if (entry.timer) return
  entry.timer = setInterval(() => {
    entry.data = emptyGraph(year)
    entry.lastFetched = Date.now()
    broadcast(year)
  }, REFRESH_MS)
}

function stopTimerIfIdle(year) {
  const entry = cache.get(year)
  if (!entry) return
  if (entry.subscribers.size === 0 && entry.timer) {
    clearInterval(entry.timer)
    entry.timer = null
  }
}

const app = express()

app.get('/api/graph', (req, res) => {
  const year = String(req.query.year || '')
  res.json(ensureEntry(year).data)
})

app.get('/api/stream', (req, res) => {
  const year = String(req.query.year || '')
  res.set({
    'Content-Type': 'text/event-stream',
    'Cache-Control': 'no-cache, no-transform',
    Connection: 'keep-alive',
    'X-Accel-Buffering': 'no',
  })
  res.flushHeaders?.()
  req.socket.setKeepAlive(true)
  req.socket.setNoDelay(true)
  req.socket.setTimeout(0)

  const entry = ensureEntry(year)
  entry.subscribers.add(res)
  broadcast(year)

  const keepalive = setInterval(() => {
    try { res.write(`: keepalive ${Date.now()}\n\n`) } catch {}
  }, 25000)

  startTimer(year)
  req.on('close', () => {
    clearInterval(keepalive)
    entry.subscribers.delete(res)
    stopTimerIfIdle(year)
  })
})

const httpServer = http.createServer(app)

const vite = await createViteServer({
  root: __dirname,
  server: {
    middlewareMode: true,
    hmr: { server: httpServer, port: PORT },
  },
  appType: 'spa',
})
app.use(vite.middlewares)

httpServer.listen(PORT, () => {
  console.log(`[tokcat] dev server listening on http://localhost:${PORT}`)
})
