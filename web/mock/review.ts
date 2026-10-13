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

function initialState(head: string): MockState {
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

  st.pendingReview = {
    id: 'PRR_mock_pending',
    body: '',
    commit: head,
    createdAt: at(),
    url: `${PR_URL}#pullrequestreview-1`,
  }
  return st
}

let state: MockState | null = null

/** The review state, built on first use (the pending review starts on `head`). */
export function reviewState(head: string): MockState {
  state ??= initialState(head)
  return state
}

export function resetReview(): void {
  state = null
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
