// Review threads of the fake PR: both sides, ranges, file level, outdated, resolved, pending,
// a long thread, a ghost author, a minimized comment, a renamed file's old path and a comment
// whose GitHub HTML carries script (must stay inert). Line numbers are looked up in the
// fixture files so they follow edits to data.ts.
import { fileLines } from './data.ts'
import { XSS_BODY_HTML } from './xss.ts'

type Side = 'LEFT' | 'RIGHT'

export interface MockComment {
  id: string
  author: { login: string; avatarUrl: string | null } | null
  body: string
  bodyHtml: string
  createdAt: string
  lastEditedAt: string | null
  url: string
  state: 'PENDING' | 'SUBMITTED'
  reviewId: string | null
  replyTo: string | null
  isMinimized: boolean
  minimizedReason: string | null
  viewerCanUpdate: boolean
  viewerCanDelete: boolean
  viewerDidAuthor: boolean
}

export interface MockThread {
  id: string
  path: string
  subjectType: 'LINE' | 'FILE'
  side: Side | null
  startSide: Side | null
  line: number | null
  startLine: number | null
  originalLine: number | null
  originalStartLine: number | null
  diffHunk: string
  isResolved: boolean
  isOutdated: boolean
  isCollapsed: boolean
  resolvedBy: string | null
  viewerCanReply: boolean
  viewerCanResolve: boolean
  viewerCanUnresolve: boolean
  comments: MockComment[]
}

export interface MockPendingReview {
  id: string
  body: string
  commit: string | null
  createdAt: string
  url: string
}

export const VIEWER = 'mock-reviewer'
const PR_URL = 'https://github.com/acme/demo-app/pull/42'

export function lineOf(path: string, side: Side, needle: string): number {
  const i = fileLines(path, side).findIndex((l) => l.includes(needle))
  if (i < 0) throw new Error(`mock: ${needle} not found in ${side} ${path}`)
  return i + 1
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`)
}

/** Paragraphs of escaped text, as GitHub renders plain markdown. */
export function renderBody(body: string): string {
  return body
    .split(/\n{2,}/)
    .map((p) => `<p dir="auto">${escapeHtml(p).replace(/\n/g, '<br>\n')}</p>`)
    .join('\n')
}

export interface MockState {
  threads: MockThread[]
  pendingReview: MockPendingReview | null
  nextId: number
}

let minute = 0
function at(): string {
  minute += 7
  return new Date(Date.UTC(2026, 9, 10, 9, 0) + minute * 60_000).toISOString().replace('.000', '')
}

interface CommentSpec {
  author?: string | null
  body: string
  html?: string
  pending?: boolean
  minimized?: string
  edited?: boolean
}

function initialState(head: string, withPending: boolean): MockState {
  minute = 0
  let n = 0
  const st: MockState = { threads: [], pendingReview: null, nextId: 1000 }
  const comment = (threadUrl: string, c: CommentSpec, replyTo: string | null): MockComment => {
    const id = `PRRC_mock_${++n}`
    const login = c.author === undefined ? 'octocat' : c.author
    const mine = login === VIEWER
    return {
      id,
      author: login === null ? null : { login, avatarUrl: null },
      body: c.body,
      bodyHtml: c.html ?? renderBody(c.body),
      createdAt: at(),
      lastEditedAt: c.edited ? at() : null,
      url: `${threadUrl}#discussion_r${n}`,
      state: c.pending ? 'PENDING' : 'SUBMITTED',
      reviewId: c.pending ? 'PRR_mock_pending' : `PRR_mock_r${n}`,
      replyTo,
      isMinimized: Boolean(c.minimized),
      minimizedReason: c.minimized ?? null,
      viewerCanUpdate: mine,
      viewerCanDelete: mine,
      viewerDidAuthor: mine,
    }
  }
  const thread = (
    path: string,
    where:
      | { side: Side; line: number; startLine?: number; startSide?: Side; originalLine?: number }
      | 'FILE'
      | { outdated: number; outdatedStart?: number },
    comments: CommentSpec[],
    opts: { resolved?: boolean; hunk?: string } = {},
  ): void => {
    const id = `PRRT_mock_${st.threads.length + 1}`
    const list: MockComment[] = []
    for (const c of comments) list.push(comment(PR_URL + '/files', c, list[0]?.id ?? null))
    const allPending = list.every((c) => c.state === 'PENDING')
    const base = {
      id,
      path,
      diffHunk: opts.hunk ?? '',
      isResolved: Boolean(opts.resolved),
      isCollapsed: Boolean(opts.resolved),
      resolvedBy: opts.resolved ? 'octocat' : null,
      viewerCanReply: true,
      viewerCanResolve: !opts.resolved && !allPending,
      viewerCanUnresolve: Boolean(opts.resolved),
      comments: list,
    }
    if (where === 'FILE') {
      st.threads.push({ ...base, subjectType: 'FILE', side: null, startSide: null, line: null, startLine: null, originalLine: null, originalStartLine: null, isOutdated: false })
    } else if ('outdated' in where) {
      const start = where.outdatedStart ?? null
      st.threads.push({
        ...base,
        subjectType: 'LINE',
        side: 'RIGHT',
        startSide: start === null ? null : 'RIGHT',
        line: null,
        startLine: null,
        originalLine: where.outdated,
        originalStartLine: start,
        isOutdated: true,
      })
    } else {
      const start = where.startLine ?? null
      // A later push moved the line: `originalLine` is where it was first commented.
      const shift = where.originalLine === undefined ? 0 : where.originalLine - where.line
      st.threads.push({
        ...base,
        subjectType: 'LINE',
        side: where.side,
        startSide: start === null ? null : (where.startSide ?? where.side),
        line: where.line,
        startLine: start,
        originalLine: where.line + shift,
        originalStartLine: start === null ? null : start + shift,
        isOutdated: false,
      })
    }
  }

  const main = 'src/app/main.ts'
  const port = lineOf(main, 'RIGHT', 'const DEFAULT_PORT = 7457')
  thread(main, { side: 'RIGHT', line: port }, [
    { body: 'Why 7457? A comment next to the constant would help.' },
    { author: 'alice', body: 'It spells TIDE-ish on a phone keypad. Agreed it needs a comment.' },
    { author: VIEWER, body: 'I will add one in a follow-up.', pending: true },
  ])
  thread(main, { side: 'LEFT', line: lineOf(main, 'LEFT', 'const DEFAULT_PORT = 8080') }, [
    { author: 'alice', body: 'Anyone relying on 8080 will break; worth a changelog entry.' },
  ])
  const slug = lineOf(main, 'RIGHT', "if (url.pathname.startsWith('/p/'))")
  thread(main, { side: 'RIGHT', startLine: slug, line: slug + 5 }, [
    {
      body: 'This redirect should be a 301 once the slugs are stable:\n\n```ts\nres.writeHead(301, { location })\n```',
      html:
        '<p dir="auto">This redirect should be a 301 once the slugs are stable:</p>\n' +
        '<div class="highlight highlight-source-ts"><pre><code>res.writeHead(301, { location })</code></pre></div>\n' +
        '<p dir="auto">See <a href="https://developer.mozilla.org/docs/Web/HTTP/Status/301" rel="nofollow">MDN</a> <img src="https://github.githubassets.com/images/icons/emoji/unicode/1f44d.png" alt=":+1:" class="emoji" width="20" height="20"></p>',
    },
  ])
  thread(
    main,
    { side: 'RIGHT', line: lineOf(main, 'RIGHT', "warn('not found: ' + url.pathname)") },
    [{ author: null, body: 'Log the method too?' }, { body: 'Done in the next commit.' }],
    { resolved: true },
  )
  const idle = lineOf(main, 'RIGHT', 'server.setTimeout(options.idleTimeoutMs)')
  thread(main, { side: 'RIGHT', line: idle }, [
    { author: 'alice', body: 'setTimeout is the socket timeout, not an idle-server timeout.' },
    { body: 'Hm, you are right. What do you suggest?' },
    { author: 'alice', body: 'Track the last request time and close the server from an interval.' },
    { author: 'spam-bot', body: 'Buy cheap followers!!!', minimized: 'spam' },
    { body: 'That needs a timer per server. Fine for this app.' },
    { author: 'alice', body: 'Also clear the interval on close.' },
    { body: 'Will do.', edited: true },
    { author: 'alice', body: 'Thanks!' },
  ])
  // A thread on an unchanged line far from any change: its region starts collapsed.
  thread(main, { side: 'RIGHT', line: lineOf(main, 'RIGHT', 'export function helper20(') }, [
    { author: 'alice', body: 'Unrelated, but helper20 is unused.' },
  ])
  // A range from a removed line to the line that replaced it.
  thread(main, { side: 'RIGHT', startSide: 'LEFT', startLine: lineOf(main, 'LEFT', 'const DEFAULT_PORT = 8080'), line: port }, [
    { author: 'alice', body: 'Old and new value side by side: the port change is intended?' },
  ])
  thread(main, { side: 'RIGHT', line: lineOf(main, 'RIGHT', 'export function helper3('), originalLine: 41 }, [
    { body: 'helper3 moved down after the last push; the comment followed it.' },
  ])
  thread(main, { outdated: 52, outdatedStart: 49 }, [{ body: 'This whole range was rewritten.' }])
  thread(main, { outdated: 40 }, [{ body: 'This block moved; is the old order still needed?' }], {
    hunk: "@@ -36,6 +38,8 @@ const DEFAULT_PORT = 8080\n export function helper0(value: number): number {\n-  return value\n+  // helper 0 keeps the example long enough to fold unchanged regions\n+  return value * 1 + 0\n }",
  })

  const strings = 'src/app/util/strings.ts'
  thread(strings, { side: 'RIGHT', line: lineOf(strings, 'RIGHT', 'export function truncate') }, [
    { author: VIEWER, body: 'truncate should count code points, not UTF-16 units.', pending: true },
  ])

  thread('src/styles/theme.css', 'FILE', [
    { author: 'alice', body: 'Can we keep the theme tokens in one file?' },
    { body: 'They are; this is that file.' },
  ])

  const user = 'src/server/api/v1/handlers/user.rs'
  thread(user, { side: 'RIGHT', line: lineOf(user, 'RIGHT', 'pub email: Option<String>,') }, [
    {
      author: 'mallory',
      body: 'Looks fine <script>alert(1)</script>',
      html: XSS_BODY_HTML,
    },
  ])

  thread('src/legacy/oldApi.js', { side: 'LEFT', line: lineOf('src/legacy/oldApi.js', 'LEFT', "var request = require('request');") }, [
    { author: 'alice', body: 'Good riddance.' },
  ])

  const button = 'src/components/Button.tsx'
  thread(button, { side: 'RIGHT', line: lineOf(button, 'RIGHT', "variant?: 'primary' | 'plain'") }, [
    { body: 'Should `variant` default to primary?' },
  ])
  // GitHub stores a LEFT comment on a renamed file under the old path when it was sent that way.
  thread('src/components/Btn.tsx', { side: 'LEFT', line: lineOf(button, 'LEFT', 'export function Btn(') }, [
    { author: 'alice', body: 'The old name was clearer in the call sites.' },
  ])

  thread('assets/logo.png', 'FILE', [{ author: 'alice', body: 'The new logo is a bit blurry at 1x.' }])

  if (withPending) {
    st.pendingReview = {
      id: 'PRR_mock_pending',
      body: '',
      commit: head,
      createdAt: at(),
      url: `${PR_URL}#pullrequestreview-1`,
    }
  } else {
    dropPending(st)
  }
  return st
}

/** Takes the viewer's pending comments out (threads left without comments go too). */
function dropPending(st: MockState): void {
  st.threads = st.threads
    .map((t) => ({ ...t, comments: t.comments.filter((c) => c.state !== 'PENDING') }))
    .filter((t) => t.comments.length > 0)
}

let state: MockState | null = null
let startPending = true
/** Test hook: the next resolve, edit or delete fails with this status (`after`: once it is done). */
let failNext: { status: number; after: boolean } | null = null

/** The review state, built on first use (the pending review starts on `head`). */
export function reviewState(head: string): MockState {
  state ??= initialState(head, startPending)
  return state
}

/** Back to the fixtures; `pending: false` starts without the viewer's pending review. */
export function resetReview(opts: { pending?: boolean } = {}): void {
  state = null
  startPending = opts.pending ?? true
  failNext = null
}

/**
 * `POST /api/__mock/fail-next {status, after?}`: the next resolve, edit or delete fails that
 * way (422 rejected, 404 not_found, others GitHub failing: 502), with `after` once it is done.
 */
export function failNextWrite(status: number, after = false): void {
  failNext = { status, after }
}

const TIMED_OUT = 'GraphQL request to github.com failed: operation timed out'

/** The failure `failNextWrite` asked for, once: `before` fails at once, `after` once the write is done. */
function takeFailNext(): { before: MockResult | null; after: MockResult | null } {
  const f = failNext
  failNext = null
  if (f === null) return { before: null, after: null }
  const result =
    f.status === 422
      ? fail(422, 'rejected', 'GitHub refused the change')
      : f.status === 404
        ? fail(404, 'not_found', 'not found')
        : fail(502, 'github_error', TIMED_OUT)
  return f.after ? { before: null, after: result } : { before: result, after: null }
}

export function threadsResponse(head: string): {
  head: string
  locked: boolean
  pendingReview: MockPendingReview | null
  threads: MockThread[]
} {
  const st = reviewState(head)
  return { head, locked: false, pendingReview: st.pendingReview, threads: st.threads }
}

export interface MockResult {
  status: number
  body: unknown
}

function fail(status: number, code: string, message: string): MockResult {
  return { status, body: { error: { code, message } } }
}

const LINE_REJECTED = 'GitHub only takes comments on lines in the diff, or within 3 lines of a change'
const THREAD_FIELDS = new Set(['head', 'path', 'body', 'mode', 'subjectType', 'side', 'line', 'startSide', 'startLine'])
const isSide = (v: unknown): v is Side => v === 'LEFT' || v === 'RIGHT'
const isLine = (v: unknown): v is number => typeof v === 'number' && Number.isInteger(v) && v >= 1

/** `/fail 422|409|502|502-after|submit` in a comment makes the write fail that way. */
function injected(body: string): string | null {
  return /\/fail (422|409|502-after|502|submit)\b/.exec(body)?.[1] ?? null
}

function newComment(st: MockState, body: string, reviewId: string, pending: boolean): MockComment {
  const n = st.nextId++
  return {
    id: `PRRC_mock_new${n}`,
    author: { login: VIEWER, avatarUrl: null },
    body,
    bodyHtml: renderBody(body),
    createdAt: new Date().toISOString().replace(/\.\d+Z$/, 'Z'),
    lastEditedAt: null,
    url: `${PR_URL}/files#discussion_r${n}`,
    state: pending ? 'PENDING' : 'SUBMITTED',
    reviewId,
    replyTo: null,
    isMinimized: false,
    minimizedReason: null,
    viewerCanUpdate: true,
    viewerCanDelete: true,
    viewerDidAuthor: true,
  }
}

function startReview(st: MockState, head: string): MockPendingReview {
  const n = st.nextId++
  st.pendingReview = { id: `PRR_mock_new${n}`, body: '', commit: head, createdAt: new Date().toISOString(), url: `${PR_URL}#pullrequestreview-${n}` }
  return st.pendingReview
}

/**
 * `POST /threads`, checked like the server does (fields, lines, path, pending review, head).
 * `listed`: GitHub's file list. GitHub's own check of the lines is left out: `/fail 422`.
 */
export function createThread(head: string, listed: (path: string) => boolean, raw: unknown): MockResult {
  if (!raw || typeof raw !== 'object') return fail(400, 'bad_request', 'invalid request body')
  const req = raw as Record<string, unknown>
  const unknown = Object.keys(req).find((k) => !THREAD_FIELDS.has(k))
  if (unknown) return fail(400, 'bad_request', `invalid request body: unknown field \`${unknown}\``)
  const { path, body, mode, subjectType, side, line, startSide, startLine } = req
  if (typeof req.head !== 'string' || !/^[0-9a-f]{40}$/.test(req.head)) return fail(400, 'bad_request', 'head must be 40 lowercase hex characters')
  if (typeof path !== 'string' || typeof body !== 'string' || (mode !== 'single' && mode !== 'review')) {
    return fail(400, 'bad_request', 'invalid request body')
  }
  if (body.trim() === '') return fail(400, 'bad_request', 'the comment is empty')
  if ([...body].length > 65536) return fail(400, 'bad_request', 'the comment is longer than 65536 characters')
  if (subjectType === 'FILE') {
    if (side != null || line != null || startSide != null || startLine != null) return fail(400, 'bad_request', 'a comment on a file has no side or lines')
  } else if (subjectType === 'LINE') {
    if (!isSide(side) || !isLine(line)) return fail(400, 'bad_request', 'a comment on lines needs a side and a line')
    if (startLine != null && (!isLine(startLine) || startLine >= line)) return fail(400, 'bad_request', `invalid range of lines ${startLine} to ${line}`)
    if (startSide != null && (!isSide(startSide) || startSide !== side)) return fail(400, 'bad_request', 'a range of lines must be on one side')
    if (startSide != null && startLine == null) return fail(400, 'bad_request', 'a start side needs a start line')
  } else {
    return fail(400, 'bad_request', 'invalid request body')
  }
  if (!listed(path)) return fail(400, 'bad_request', `${JSON.stringify(path)} is not a file of this pull request`)

  const st = reviewState(head)
  if (mode === 'single' && st.pendingReview) {
    return fail(409, 'pending_review_exists', 'you have a pending review: add the comment to it, or submit or discard it first')
  }
  const how = injected(body)
  if (how === '409' || ((subjectType === 'LINE' || !st.pendingReview) && req.head !== head)) {
    return fail(409, 'stale_head', 'the pull request has new commits; refresh before commenting')
  }
  if (how === '422') return fail(422, 'rejected', subjectType === 'FILE' ? 'GitHub did not take a comment on this file' : LINE_REJECTED)
  if (how === '502') return fail(502, 'github_error', TIMED_OUT)

  // A single comment on a file is added to a review and then submitted (comments on lines go in
  // one call): `/fail submit` leaves that review pending.
  const stuck = how === 'submit' && mode === 'single' && subjectType === 'FILE'
  const pending = mode === 'review' || stuck
  const review = pending ? (st.pendingReview ?? startReview(st, head)) : null
  const comment = newComment(st, body, review?.id ?? `PRR_mock_new${st.nextId++}`, pending)
  const lines = subjectType === 'LINE'
  const thread: MockThread = {
    id: `PRRT_mock_new${st.nextId++}`,
    path,
    subjectType: lines ? 'LINE' : 'FILE',
    side: lines ? (side as Side) : null,
    startSide: lines && startLine != null ? (side as Side) : null,
    line: lines ? (line as number) : null,
    startLine: lines ? ((startLine as number | undefined) ?? null) : null,
    originalLine: lines ? (line as number) : null,
    originalStartLine: lines ? ((startLine as number | undefined) ?? null) : null,
    diffHunk: '',
    isResolved: false,
    isOutdated: false,
    isCollapsed: false,
    resolvedBy: null,
    viewerCanReply: true,
    viewerCanResolve: !pending,
    viewerCanUnresolve: false,
    comments: [comment],
  }
  st.threads.push(thread)
  if (how === '502-after') return fail(502, 'github_error', TIMED_OUT)
  return {
    status: 200,
    body: {
      thread,
      pendingReview: st.pendingReview,
      ...(stuck ? { submitError: 'Something went wrong while executing your query.' } : {}),
    },
  }
}

/** `DELETE /review {reviewId}`: the pending review shown goes with its comments. */
export function discardReview(head: string, raw: unknown): MockResult {
  const id = (raw as { reviewId?: unknown } | null)?.reviewId
  if (typeof id !== 'string' || !/^[A-Za-z0-9_=+/-]{1,128}$/.test(id)) return fail(400, 'bad_request', 'invalid id')
  const st = reviewState(head)
  if (st.pendingReview && st.pendingReview.id !== id) {
    return fail(409, 'review_changed', 'your pending review on GitHub is not the one shown; refresh to see it')
  }
  st.pendingReview = null
  dropPending(st)
  return { status: 200, body: { pendingReview: null } }
}

const REPLY_FIELDS = new Set(['head', 'body', 'mode'])
const goneThread = () => fail(404, 'not_found', 'this thread is no longer on GitHub; refresh to see the rest')
const goneComment = () => fail(404, 'not_found', 'this comment is no longer on GitHub; refresh to see the rest')

function checkBody(body: unknown): MockResult | null {
  if (typeof body !== 'string') return fail(400, 'bad_request', 'invalid request body')
  if (body.trim() === '') return fail(400, 'bad_request', 'the comment is empty')
  if ([...body].length > 65536) return fail(400, 'bad_request', 'the comment is longer than 65536 characters')
  return null
}

/** `POST /threads/{t}/replies`, checked like the server does (a reply has no lines). */
export function reply(head: string, threadId: string, raw: unknown): MockResult {
  if (!raw || typeof raw !== 'object') return fail(400, 'bad_request', 'invalid request body')
  const req = raw as Record<string, unknown>
  const unknown = Object.keys(req).find((k) => !REPLY_FIELDS.has(k))
  if (unknown) return fail(400, 'bad_request', `invalid request body: unknown field \`${unknown}\``)
  const { body, mode } = req
  if (typeof req.head !== 'string' || !/^[0-9a-f]{40}$/.test(req.head)) return fail(400, 'bad_request', 'head must be 40 lowercase hex characters')
  if (mode !== 'single' && mode !== 'review') return fail(400, 'bad_request', 'invalid request body')
  const bad = checkBody(body)
  if (bad) return bad
  const text = body as string

  const st = reviewState(head)
  if (mode === 'single' && st.pendingReview) {
    return fail(409, 'pending_review_exists', 'you have a pending review: add the comment to it, or submit or discard it first')
  }
  const how = injected(text)
  if (how === '409' || (!st.pendingReview && req.head !== head)) {
    return fail(409, 'stale_head', 'the pull request has new commits; refresh before commenting')
  }
  const thread = st.threads.find((t) => t.id === threadId)
  if (!thread) return goneThread()
  if (how === '422') return fail(422, 'rejected', 'GitHub refused the reply')
  if (how === '502') return fail(502, 'github_error', TIMED_OUT)

  const pending = mode === 'review'
  const review = pending ? (st.pendingReview ?? startReview(st, head)) : null
  const comment = { ...newComment(st, text, review?.id ?? `PRR_mock_new${st.nextId++}`, pending), replyTo: thread.comments[0]?.id ?? null }
  thread.comments.push(comment)
  if (how === '502-after') return fail(502, 'github_error', TIMED_OUT)
  return { status: 200, body: { comment, pendingReview: st.pendingReview } }
}

/** `PUT /threads/{t}/resolved {resolved}`. */
export function setResolved(head: string, threadId: string, raw: unknown): MockResult {
  const resolved = (raw as { resolved?: unknown } | null)?.resolved
  if (typeof resolved !== 'boolean' || Object.keys(raw as object).length !== 1) return fail(400, 'bad_request', 'expected {resolved}')
  const failed = takeFailNext()
  if (failed.before) return failed.before
  const thread = reviewState(head).threads.find((t) => t.id === threadId)
  if (!thread) return goneThread()
  Object.assign(thread, {
    isResolved: resolved,
    isCollapsed: resolved,
    resolvedBy: resolved ? VIEWER : null,
    viewerCanResolve: !resolved,
    viewerCanUnresolve: resolved,
  })
  return failed.after ?? { status: 200, body: { thread } }
}

/**
 * `PATCH /threads/{t}/comments/{c} {body}`; GitHub marks only submitted comments edited. The
 * comment is found by its id alone, as on GitHub.
 */
export function editComment(head: string, commentId: string, raw: unknown): MockResult {
  const body = (raw as { body?: unknown } | null)?.body
  if (Object.keys((raw as object | null) ?? {}).some((k) => k !== 'body')) return fail(400, 'bad_request', 'expected {body}')
  const bad = checkBody(body)
  if (bad) return bad
  const text = body as string
  const failed = takeFailNext()
  if (failed.before) return failed.before
  const how = injected(text)
  if (how === '422') return fail(422, 'rejected', 'GitHub refused the change')
  if (how === '502') return fail(502, 'github_error', TIMED_OUT)
  const comment = reviewState(head)
    .threads.flatMap((t) => t.comments)
    .find((c) => c.id === commentId)
  if (!comment) return goneComment()
  Object.assign(comment, {
    body: text,
    bodyHtml: renderBody(text),
    lastEditedAt: comment.state === 'SUBMITTED' ? new Date().toISOString().replace(/\.\d+Z$/, 'Z') : comment.lastEditedAt,
  })
  if (how === '502-after') return fail(502, 'github_error', TIMED_OUT)
  return failed.after ?? { status: 200, body: { comment } }
}

/**
 * `DELETE /threads/{t}/comments/{c}`: one gone already counts as deleted. A thread goes with
 * its last comment, and the pending review with its last pending comment.
 */
export function deleteComment(head: string, threadId: string, commentId: string): MockResult {
  const failed = takeFailNext()
  if (failed.before) return failed.before
  const st = reviewState(head)
  const thread = st.threads.find((t) => t.id === threadId)
  const comment = thread?.comments.find((c) => c.id === commentId)
  if (thread && comment) {
    thread.comments = thread.comments.filter((c) => c !== comment)
    if (thread.comments.length === 0) st.threads = st.threads.filter((t) => t !== thread)
    if (comment.state === 'PENDING' && !st.threads.some((t) => t.comments.some((c) => c.state === 'PENDING'))) st.pendingReview = null
  }
  // As the server says when reading the thread back fails.
  if (failed.after) return fail(502, 'github_error', `the comment was deleted, but reading its thread back failed: ${TIMED_OUT}`)
  const left = st.threads.find((t) => t.id === threadId) ?? null
  return { status: 200, body: { thread: left, pendingReview: st.pendingReview } }
}
