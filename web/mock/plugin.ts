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
      const review = await import('./review.ts')
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
        // Test hook: the review state (and the head) back to the fixtures (`{"pending": false}`: no
        // pending review; `{"staleReview": true}`: the pending review on a commit before the head).
        if (url.pathname === '/api/__mock/reset' && req.method === 'POST') {
          void readJson(req).then((body) => {
            const o = body as { pending?: unknown; staleReview?: unknown } | null
            data.setHead(null)
            review.resetReview({ pending: o?.pending !== false, staleReview: o?.staleReview === true })
            json(res, 200, { ok: true })
          })
          return
        }
        // Test hook: new commits (the same files) on the pull request.
        if (url.pathname === '/api/__mock/push' && req.method === 'POST') {
          data.setHead('e'.repeat(40))
          return json(res, 200, { ok: true })
        }
        if (url.pathname === '/api/__mock/fail-next' && req.method === 'POST') {
          void readJson(req).then((body) => {
            const { status = 502, after = false } = (body ?? {}) as { status?: unknown; after?: unknown }
            if (typeof status !== 'number' || !Number.isInteger(status) || typeof after !== 'boolean') {
              return error(res, 400, 'bad_request', 'expected {status, after?}')
            }
            review.failNextWrite(status, after)
            json(res, 200, { ok: true })
          })
          return
        }
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
            if (sub === 'threads' && parts.length === 6 && req.method === 'GET') {
              await delay(250)
              return json(res, 200, review.threadsResponse(pr.head.sha))
            }
            if (sub === 'threads' && parts.length === 6 && req.method === 'POST') {
              const body = await readJson(req)
              await delay(500)
              const r = review.createThread(pr.head.sha, (p) => data.viewed.has(p), body)
              return json(res, r.status, r.body)
            }
            if (sub === 'threads' && parts.length >= 7) {
              const [threadId, part, commentId] = parts.slice(6)
              const ok = (v: string | undefined) => v !== undefined && /^[A-Za-z0-9_=+/-]{1,128}$/.test(v)
              if (!ok(threadId) || (part === 'comments' && !ok(commentId))) return error(res, 400, 'bad_request', 'invalid id')
              let r: { status: number; body: unknown } | null = null
              if (part === 'replies' && parts.length === 8 && req.method === 'POST') {
                const body = await readJson(req)
                await delay(500)
                r = review.reply(pr.head.sha, threadId, body)
              } else if (part === 'resolved' && parts.length === 8 && req.method === 'PUT') {
                const body = await readJson(req)
                await delay(400)
                r = review.setResolved(pr.head.sha, threadId, body)
              } else if (part === 'comments' && parts.length === 9 && req.method === 'PATCH') {
                const body = await readJson(req)
                await delay(400)
                r = review.editComment(pr.head.sha, commentId, body)
              } else if (part === 'comments' && parts.length === 9 && req.method === 'DELETE') {
                await delay(400)
                r = review.deleteComment(pr.head.sha, threadId, commentId)
              }
              if (r) return json(res, r.status, r.body)
            }
            if (sub === 'review' && parts.length === 6 && req.method === 'POST') {
              const body = await readJson(req)
              await delay(600)
              const r = review.submitReview(pr.head.sha, pr.author === pr.viewer, body)
              return json(res, r.status, r.body)
            }
            if (sub === 'review' && parts.length === 6 && req.method === 'DELETE') {
              const body = await readJson(req)
              await delay(400)
              const r = review.discardReview(pr.head.sha, body)
              return json(res, r.status, r.body)
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
          if (kind === 'markdown' && parts.length === 4 && req.method === 'POST') {
            const body = await readJson(req)
            await delay(300)
            const r = review.renderPreview(body)
            return json(res, r.status, r.body)
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
