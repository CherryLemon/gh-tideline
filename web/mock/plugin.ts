// Dev-only Vite plugin that answers /api/* with the fake PR from ./data.ts, following the
// server's API contract (error shape, blob headers, X-DR requirement).
// Loaded from vite.config.ts only in `--mode mock`; never part of the production build.
import type { IncomingMessage, ServerResponse } from 'node:http'
import type { Plugin } from 'vite'

function json(res: ServerResponse, status: number, body: unknown): void {
  const buf = Buffer.from(JSON.stringify(body))
  res.statusCode = status
  res.setHeader('Content-Type', 'application/json')
  res.setHeader('Content-Length', buf.length)
  res.end(buf)
}

function error(res: ServerResponse, status: number, code: string, message: string): void {
  json(res, status, { error: { code, message } })
}

async function readJson(req: IncomingMessage): Promise<unknown> {
  const chunks: Buffer[] = []
  for await (const chunk of req) chunks.push(chunk as Buffer)
  try {
    return JSON.parse(Buffer.concat(chunks).toString('utf8'))
  } catch {
    return null
  }
}

export function mockApi(): Plugin {
  return {
    name: 'tideline-mock-api',
    apply: 'serve',
    async configureServer(server) {
      const data = await import('./data.ts')
      server.config.logger.info(`  [mock] serving fake PR with ${data.files.length} files at /github.com/acme/demo-app/pull/42`)

      server.middlewares.use((req: IncomingMessage, res: ServerResponse, next: () => void) => {
        const url = new URL(req.url ?? '/', 'http://localhost')
        if (url.pathname === '/__auth') {
          const nextPath = url.searchParams.get('next') ?? '/'
          res.statusCode = 302
          res.setHeader('Location', nextPath.startsWith('/') && !nextPath.startsWith('//') ? nextPath : '/')
          res.end()
          return
        }
        if (!url.pathname.startsWith('/api/')) return next()

        if (url.pathname === '/api/health') return json(res, 200, { app: 'tideline', version: '0.1.0-mock' })
        if (req.headers['x-dr'] !== '1' && req.headers['sec-fetch-site'] !== 'same-origin') {
          return error(res, 401, 'unauthorized', 'missing X-DR header')
        }

        const parts = url.pathname.split('/').slice(2).map(decodeURIComponent)
        const [host, owner, repo, kind, id, sub] = parts
        if (host !== 'github.com') return error(res, 403, 'forbidden_host', `host ${host} is not allowed`)

        const delay = (ms: number) => new Promise((r) => setTimeout(r, ms))
        void (async () => {
          if (kind === 'pulls' && id) {
            const n = Number(id)
            if (n === 401) return error(res, 401, 'unauthorized', 'no session')
            if (n === 404) return error(res, 404, 'not_found', `PR ${owner}/${repo}#${n} not found`)
            if (n === 502) return error(res, 502, 'github_error', 'GraphQL: rate limit exceeded')
            await delay(120)
            const pr = data.prSummary(host, owner, repo, n)
            if (sub === undefined && req.method === 'GET') return json(res, 200, pr)
            if (sub === 'files' && req.method === 'GET') return json(res, 200, data.filesWithViewed())
            if (sub === 'tree' && req.method === 'GET') {
              await delay(300)
              // Like the server: a head it has moved on from gets the new head and no entries.
              const asked = url.searchParams.get('head')
              return json(res, 200, { head: pr.head.sha, entries: asked && asked !== pr.head.sha ? [] : data.tree })
            }
            if (sub === 'viewed' && req.method === 'PUT') {
              const body = (await readJson(req)) as { path?: unknown; viewed?: unknown; head?: unknown } | null
              if (!body || typeof body.path !== 'string' || typeof body.viewed !== 'boolean' || typeof body.head !== 'string') {
                return error(res, 400, 'bad_request', 'expected {path, viewed, head}')
              }
              if (!data.viewed.has(body.path)) return error(res, 400, 'bad_request', `${body.path} is not part of the pull request`)
              await delay(400)
              if (body.head !== pr.head.sha || body.path === data.VIEWED_STALE) {
                return error(res, 409, 'stale_head', 'the pull request head has moved')
              }
              if (body.path === data.VIEWED_FAILS) return error(res, 502, 'github_error', 'GraphQL: something went wrong')
              const state = body.viewed ? 'VIEWED' : 'UNVIEWED'
              data.viewed.set(body.path, state)
              return json(res, 200, { path: body.path, viewed: state })
            }
          }
          if (kind === 'blobs' && id && sub === undefined) {
            if (!/^[0-9a-f]{40}$/.test(id)) return error(res, 400, 'bad_request', 'invalid oid')
            const blob = data.blobs.get(id)
            if (!blob) return error(res, 404, 'not_found', `blob ${id} not found`)
            await delay(30)
            res.statusCode = 200
            res.setHeader('Content-Type', 'application/octet-stream')
            res.setHeader('Content-Length', blob.length)
            res.setHeader('X-Content-Type-Options', 'nosniff')
            res.setHeader('Content-Security-Policy', 'sandbox')
            res.setHeader('Cache-Control', 'private, max-age=31536000, immutable')
            res.end(blob)
            return
          }
          error(res, 404, 'not_found', `no route for ${url.pathname}`)
        })().catch((e: unknown) => {
          // E.g. the page navigated away while a request body was still arriving.
          if (!res.headersSent && !res.destroyed) error(res, 500, 'mock_error', String(e))
        })
      })
    },
  }
}
