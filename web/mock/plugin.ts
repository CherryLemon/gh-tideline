// Dev-only Vite plugin that answers /api/* with the fake PR from ./data.ts, following the
// contract in docs/m0-skeleton.md (error shape, blob headers, X-DR requirement).
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
            if (sub === undefined) return json(res, 200, data.prSummary(host, owner, repo, n))
            if (sub === 'files') return json(res, 200, data.files)
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
        })()
      })
    },
  }
}
