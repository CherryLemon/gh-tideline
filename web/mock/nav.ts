// The mock of GET /blobs/{oid}/symbols, and of the repository index built on it. A few regular expressions per language stand in
// for the server's tree-sitter tags: at most one definition per line, with the kinds the server reports, and an extent found by
// matching braces (by indentation in Python). Good enough for the fixtures; it does not parse anything.

export interface MockSymbol {
  name: string
  kind: string
  line: number
  column: number
  endColumn: number
  startLine: number
  endLine: number
}

export interface MockFileSymbols {
  language: string | null
  tooLarge: boolean
  symbols: MockSymbol[]
}

/** Where a line sits: directly in the body of a class, impl or trait; outside every `{}`. */
interface Here {
  inContainer: boolean
  topLevel: boolean
}

interface Rule {
  /** The name is group 1 (with the `d` flag, for its columns). */
  re: RegExp
  /** `null`: a line like this defines nothing here. */
  kind: string | ((name: string, here: Here) => string | null)
  /** The body holds methods. */
  container?: boolean
}

const MAX_BYTES = 1 << 20

const LANGUAGES: Record<string, string> = {
  ts: 'typescript',
  mts: 'typescript',
  cts: 'typescript',
  tsx: 'tsx',
  js: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  jsx: 'javascript',
  rs: 'rust',
  go: 'go',
  py: 'python',
  pyi: 'python',
}

const rx = (source: string) => new RegExp(source, 'd')

const ID = '[A-Za-z_$][\\w$]*'
const EXPORT = '(?:export\\s+)?(?:default\\s+)?(?:declare\\s+)?'
const MODIFIERS = '(?:(?:public|private|protected|static|readonly|abstract|override|async|get|set|accessor)\\s+)*'
const ARROW = '(?:async\\s+)?(?:function\\b|\\([^)]*\\)\\s*(?::[^=]*)?=>|' + ID + '\\s*=>)'

const ECMA: Rule[] = [
  { re: rx(`^\\s*${EXPORT}(?:abstract\\s+)?class\\s+(${ID})`), kind: 'class', container: true },
  { re: rx(`^\\s*${EXPORT}interface\\s+(${ID})`), kind: 'interface' },
  { re: rx(`^\\s*${EXPORT}type\\s+(${ID})\\s*[=<]`), kind: 'type' },
  { re: rx(`^\\s*${EXPORT}(?:const\\s+)?enum\\s+(${ID})`), kind: 'enum' },
  { re: rx(`^\\s*${EXPORT}(?:namespace|module)\\s+(${ID})`), kind: 'module' },
  { re: rx(`^\\s*${EXPORT}(?:async\\s+)?function\\s*\\*?\\s*(${ID})`), kind: 'function' },
  { re: rx(`^\\s*${EXPORT}(?:const|let|var)\\s+(${ID})\\s*(?::[^=]*)?=\\s*${ARROW}`), kind: 'function' },
  { re: rx(`^\\s*${EXPORT}(?:const|let|var)\\s+(${ID})`), kind: (_, here) => (here.topLevel ? 'constant' : null) },
  {
    re: rx(`^\\s*${MODIFIERS}\\*?\\s*(#?${ID})\\s*(?:<[^>]*>)?\\s*\\(`),
    kind: (name, here) => (here.inContainer ? (name === 'constructor' ? 'constructor' : 'method') : null),
  },
  { re: rx(`^\\s*${MODIFIERS}(#?${ID})\\s*[?!]?\\s*(?::[^=]+)?=\\s*${ARROW}`), kind: (_, here) => (here.inContainer ? 'method' : null) },
]

const VISIBILITY = '(?:pub(?:\\([^)]*\\))?\\s+)?'
const RUST_ID = '[A-Za-z_]\\w*'

const RUST: Rule[] = [
  { re: rx(`^\\s*${VISIBILITY}(?:struct|enum|union|type)\\s+(${RUST_ID})`), kind: 'class' },
  { re: rx(`^\\s*${VISIBILITY}(?:unsafe\\s+)?trait\\s+(${RUST_ID})`), kind: 'interface', container: true },
  { re: rx(`^\\s*${VISIBILITY}(?:const|static)\\s+(?:mut\\s+)?(${RUST_ID})\\s*:`), kind: 'constant' },
  {
    re: rx(`^\\s*${VISIBILITY}(?:default\\s+)?(?:const\\s+)?(?:async\\s+)?(?:unsafe\\s+)?(?:extern(?:\\s+"[^"]*")?\\s+)?fn\\s+(${RUST_ID})`),
    kind: (_, here) => (here.inContainer ? 'method' : 'function'),
  },
  { re: rx(`^\\s*${VISIBILITY}mod\\s+(${RUST_ID})`), kind: 'module' },
  { re: rx(`^\\s*macro_rules!\\s*(${RUST_ID})`), kind: 'macro' },
  // The tag of an impl block is named after the trait, or the type for an impl without one.
  { re: rx(`^\\s*(?:unsafe\\s+)?impl(?:<.*?>)?\\s+(?:[\\w:]+::)?(${RUST_ID})(?:<.*?>)?\\s+for\\s`), kind: 'implementation', container: true },
  { re: rx(`^\\s*(?:unsafe\\s+)?impl(?:<.*?>)?\\s+(?:[\\w:]+::)?(${RUST_ID})`), kind: 'implementation', container: true },
]

const GO: Rule[] = [
  { re: rx(`^func\\s+(${RUST_ID})\\s*[(\\[]`), kind: 'function' },
  { re: rx(`^func\\s*\\([^)]*\\)\\s*(${RUST_ID})\\s*[(\\[]`), kind: 'method' },
  { re: rx(`^type\\s+(${RUST_ID})(?:\\[[^\\]]*\\])?\\s+struct\\b`), kind: 'class' },
  { re: rx(`^type\\s+(${RUST_ID})(?:\\[[^\\]]*\\])?\\s+interface\\b`), kind: 'interface' },
  { re: rx(`^type\\s+(${RUST_ID})\\b`), kind: 'type' },
  { re: rx(`^const\\s+(${RUST_ID})\\b`), kind: 'constant' },
  { re: rx(`^var\\s+(${RUST_ID})\\b`), kind: 'variable' },
]

const RULES: Record<string, Rule[]> = { typescript: ECMA, tsx: ECMA, javascript: ECMA, rust: RUST, go: GO }

function languageOf(path: string): string | null {
  const segments = path.split('/')
  const name = segments[segments.length - 1]
  const dot = name.lastIndexOf('.')
  if (segments.includes('node_modules') || name.endsWith('.min.js') || dot <= 0) return null
  return LANGUAGES[name.slice(dot + 1).toLowerCase()] ?? null
}

/** Where the string literal that starts at `i` ends (exclusive). */
function stringEnd(text: string, i: number, language: string): number {
  const quote = text[i]
  if (language === 'rust' && quote === "'") {
    // A char literal ('x', '\n'), or the apostrophe of a lifetime ('a).
    const m = /^'(?:\\[^']+|[^'\\])'/.exec(text.slice(i, i + 12))
    return m ? i + m[0].length : i + 1
  }
  const closer = language === 'python' && text.startsWith(quote.repeat(3), i) ? quote.repeat(3) : quote
  for (let j = i + closer.length; j < text.length; j++) {
    if (text[j] === '\\' && !(language === 'go' && quote === '`')) j++
    else if (text.startsWith(closer, j)) return j + closer.length
    // A string that is not closed ends with its line, apart from template literals, raw strings and docstrings.
    else if (text[j] === '\n' && closer.length === 1 && quote !== '`') return j
  }
  return text.length
}

/** `text` with the comments and strings turned into spaces (same offsets, same lines), so that only code is left to look at. */
function blank(text: string, language: string): string {
  const out = text.split('')
  const python = language === 'python'
  let i = 0
  while (i < text.length) {
    const c = text[i]
    let end = i
    if (python ? c === '#' : text.startsWith('//', i)) {
      end = text.indexOf('\n', i)
      if (end < 0) end = text.length
    } else if (!python && text.startsWith('/*', i)) {
      const close = text.indexOf('*/', i + 2)
      end = close < 0 ? text.length : close + 2
    } else if (c === '"' || c === "'" || (c === '`' && language !== 'rust' && !python)) {
      end = stringEnd(text, i, language)
    } else {
      i++
      continue
    }
    for (let k = i; k < end; k++) if (out[k] !== '\n') out[k] = ' '
    i = end
  }
  return out.join('')
}

/** Definitions in C-like code: a rule per line, and the body of each is found by matching braces. */
function braceSymbols(text: string, language: string): MockSymbol[] {
  const rules = RULES[language]
  const lines = blank(text, language).split('\n')
  const starts: number[] = []
  let offset = 0
  for (const line of lines) {
    starts.push(offset)
    offset += line.length + 1
  }
  // The `{` that most closely encloses the start of each line, and the line of the `}` of each `{`.
  const owner: Array<number | undefined> = []
  const closeLine = new Map<number, number>()
  const open: number[] = []
  lines.forEach((line, n) => {
    owner.push(open[open.length - 1])
    for (let c = 0; c < line.length; c++) {
      if (line[c] === '{') open.push(starts[n] + c)
      else if (line[c] === '}') {
        const at = open.pop()
        if (at !== undefined) closeLine.set(at, n)
      }
    }
  })

  /** The last line of the definition whose name ends at column `from` of line `n`, and its `{` if it has a body. */
  const extent = (n: number, from: number): { end: number; body?: number } => {
    let depth = 0
    for (let row = n; row < lines.length; row++) {
      const line = lines[row]
      for (let c = row === n ? from : 0; c < line.length; c++) {
        const ch = line[c]
        if (ch === '(' || ch === '[') depth++
        else if (ch === ')' || ch === ']') depth--
        else if (depth <= 0 && ch === '{') return { end: closeLine.get(starts[row] + c) ?? lines.length - 1, body: starts[row] + c }
        else if (depth <= 0 && ch === ';') return { end: row }
      }
      // Without semicolons a statement ends with its line, unless the line trails off (Rust always has a `;` or a `{`).
      if (language !== 'rust' && depth <= 0 && !/(?:[=,(&|+*/?:<.]|=>)\s*$/.test(line)) return { end: row }
    }
    return { end: lines.length - 1 }
  }

  const containers = new Set<number>()
  const symbols: MockSymbol[] = []
  lines.forEach((line, n) => {
    const at = owner[n]
    const here: Here = { inContainer: at !== undefined && containers.has(at), topLevel: at === undefined }
    for (const rule of rules) {
      const m = rule.re.exec(line)
      const span = m?.indices?.[1]
      if (!m || !span) continue
      const kind = typeof rule.kind === 'string' ? rule.kind : rule.kind(m[1], here)
      if (!kind) continue
      const { end, body } = extent(n, span[1])
      if (rule.container && body !== undefined) containers.add(body)
      symbols.push({ name: m[1], kind, line: n + 1, column: span[0] + 1, endColumn: span[1] + 1, startLine: n + 1, endLine: end + 1 })
      break
    }
  })
  return symbols
}

/** Definitions in Python: a `def` or `class` extends over the lines indented deeper than it. */
function pythonSymbols(text: string): MockSymbol[] {
  const lines = blank(text, 'python').split('\n')
  const indent = (line: string) => line.length - line.trimStart().length
  /** The last line of the statement that starts at `n`: it goes on while brackets are open. */
  const statementEnd = (n: number) => {
    let depth = 0
    for (let row = n; row < lines.length; row++) {
      for (const ch of lines[row]) depth += '([{'.includes(ch) ? 1 : ')]}'.includes(ch) ? -1 : 0
      if (depth <= 0) return row
    }
    return lines.length - 1
  }
  const scopes: Array<{ indent: number; kind: string }> = []
  const symbols: MockSymbol[] = []
  lines.forEach((line, n) => {
    const def = /^(\s*)(?:async\s+)?(def|class)\s+([A-Za-z_]\w*)/d.exec(line)
    const assignment = /^([A-Za-z_]\w*)\s*(?::[^=]+)?=(?!=)/d.exec(line)
    const m = def ?? assignment
    const span = m?.indices?.[def ? 3 : 1]
    if (!m || !span) return
    const name = def ? m[3] : m[1]
    let kind = 'constant'
    let end = statementEnd(n)
    if (def) {
      while (scopes.length > 0 && scopes[scopes.length - 1].indent >= m[1].length) scopes.pop()
      kind = def[2] === 'class' ? 'class' : scopes[scopes.length - 1]?.kind === 'class' ? 'method' : 'function'
      scopes.push({ indent: m[1].length, kind })
      for (let row = end + 1; row < lines.length && (lines[row].trim() === '' || indent(lines[row]) > m[1].length); row++) {
        if (lines[row].trim() !== '') end = row
      }
    }
    symbols.push({ name, kind, line: n + 1, column: span[0] + 1, endColumn: span[1] + 1, startLine: n + 1, endLine: end + 1 })
  })
  return symbols
}

export function fileSymbols(path: string, content: Buffer): MockFileSymbols {
  const language = languageOf(path)
  if (!language) return { language: null, tooLarge: false, symbols: [] }
  if (content.length > MAX_BYTES) return { language, tooLarge: true, symbols: [] }
  let text = content.toString('utf8')
  if (text.charCodeAt(0) === 0xfeff) text = text.slice(1)
  const symbols = language === 'python' ? pythonSymbols(text) : braceSymbols(text, language)
  return { language, tooLarge: false, symbols: symbols.sort((a, b) => a.startLine - b.startLine || a.column - b.column) }
}

// --- The index: build jobs, definitions and references ----------------------------------------------------------------------

export type NavSide = 'head' | 'base'
type IndexState = 'missing' | 'queued' | 'building' | 'ready' | 'failed' | 'cancelled'
type IndexPhase = 'listing' | 'fetching' | 'parsing' | 'saving'

export interface MockIndexStatus {
  commit: string
  state: IndexState
  phase: IndexPhase | null
  done: number
  total: number
  cancelling: boolean
  message: string | null
}

export interface MockHit {
  path: string
  line: number
  column: number
  endColumn: number
  kind: string
  snippet: string
  snippetColumn: number
  tier: 'file' | 'dir' | 'other'
}

export interface MockNavResult {
  language: string | null
  commit: string
  name: string
  hits: MockHit[]
  truncated: boolean
}

/** A commit of the pull request: its tree, as `git ls-tree -r` lists it. */
export interface NavCommit {
  sha: string
  tree: ReadonlyArray<{ path: string; oid: string }>
}

/** What the index routes need to know of the pull request at the time of a request. */
export interface NavPull {
  /** The head the pull request has now: a request for another one is stale. */
  head: string
  commits: Record<NavSide, NavCommit>
}

export interface MockResponse {
  status: number
  body: unknown
}

/** How much of a build each phase takes, in order. The files are counted while they are fetched and parsed. */
const PHASES: Array<[IndexPhase, number]> = [
  ['listing', 0.1],
  ['fetching', 0.45],
  ['parsing', 0.35],
  ['saving', 0.1],
]
/** A job waits this long before it starts building. */
const QUEUED_MS = 80
/** A job that is told to stop takes this long to. */
const CANCEL_MS = 100
/** A build that is to fail does so at this share of its time. */
const FAIL_AT = 0.4
const MAX_DEFINITIONS = 200
const MAX_REFERENCES = 1000
const MAX_SNIPPET = 200

interface Job {
  startedAt: number
  durationMs: number
  /** The files the build counts. */
  files: number
  /** When the job was told to stop. */
  cancelledAt: number | null
  /** The message of the failure this build ends in. */
  failure: string | null
}

/** A file of a commit, read for a look-up. */
interface Parsed {
  text: string
  lines: string[]
  language: string
  symbols: MockSymbol[]
  /** The lines with comments and strings blanked: only code is left to look for names in. */
  code?: string[]
}

const response = (status: number, body: unknown): MockResponse => ({ status, body })
const failure = (status: number, code: string, message: string) => response(status, { error: { code, message } })

/** Lookups match within a family of languages: JavaScript and TypeScript see each other. */
const familyOf = (language: string) => (language === 'typescript' || language === 'tsx' || language === 'javascript' ? 'ecma' : language)

const dirOf = (path: string) => path.slice(0, path.lastIndexOf('/') + 1)

/** The number of leading directories two paths have in common. */
function commonDirs(a: string, b: string): number {
  const x = dirOf(a).split('/')
  const y = dirOf(b).split('/')
  let n = 0
  while (n < x.length - 1 && n < y.length - 1 && x[n] === y[n]) n++
  return n
}

const TIERS = ['file', 'dir', 'other']

const tierOf = (path: string, from: string): MockHit['tier'] => (path === from ? 'file' : dirOf(path) === dirOf(from) ? 'dir' : 'other')

const byPath = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0)

/** The line, trimmed and cut like the server does, and the column of the name in it. */
function snippetOf(line: string, column: number): { snippet: string; snippetColumn: number } {
  let snippet = line.trim()
  if (snippet.length > MAX_SNIPPET) {
    // Not in the middle of a character made of two UTF-16 units.
    const high = (snippet.charCodeAt(MAX_SNIPPET - 1) & 0xfc00) === 0xd800
    snippet = snippet.slice(0, high ? MAX_SNIPPET - 1 : MAX_SNIPPET)
  }
  return { snippet, snippetColumn: column - (line.length - line.trimStart().length) }
}

const escapeRegExp = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')

/** The builds of the index of a pull request's commits, and the looking up of names in the commits that are built. */
export class MockIndex {
  private readonly jobs = new Map<string, Job>()
  private readonly parsed = new Map<string, Parsed | null>()
  private durationMs = 1500
  private nextFailure: string | null = null
  private unavailable = false

  constructor(private readonly blob: (oid: string) => Buffer | undefined) {}

  /** Test hook: every commit unindexed again; the builds to come take `durationMs`. */
  reset(options: { durationMs?: unknown; unavailable?: unknown } | null): void {
    const ms = options?.durationMs
    this.jobs.clear()
    this.durationMs = typeof ms === 'number' && ms >= 0 && ms <= 60_000 ? ms : 1500
    this.nextFailure = null
    this.unavailable = options?.unavailable === true
  }

  /** Test hook: the next build to start fails, part of the way through. */
  failNext(message: string): void {
    this.nextFailure = message
  }

  private statusOf(job: Job | undefined, commit: string, now: number): MockIndexStatus {
    const idle = { commit, phase: null, done: 0, total: 0, cancelling: false, message: null }
    if (!job) return { ...idle, state: 'missing' }
    if (job.cancelledAt !== null && now - job.cancelledAt >= CANCEL_MS) return { ...idle, state: 'cancelled' }
    // The progress stops when the job is told to.
    const elapsed = (job.cancelledAt ?? now) - job.startedAt
    const share = job.durationMs > 0 ? elapsed / job.durationMs : 1
    if (job.failure !== null && share >= FAIL_AT) return { ...idle, state: 'failed', message: job.failure }
    if (share >= 1) return { ...idle, state: 'ready' }
    const cancelling = job.cancelledAt !== null
    if (elapsed < QUEUED_MS) return { ...idle, state: 'queued', cancelling }
    let from = 0
    for (const [phase, span] of PHASES) {
      if (share < from + span) {
        const counted = phase === 'fetching' || phase === 'parsing'
        return { ...idle, state: 'building', cancelling, phase, done: counted ? Math.floor(((share - from) / span) * job.files) : 0, total: counted ? job.files : 0 }
      }
      from += span
    }
    return { ...idle, state: 'building', cancelling, phase: 'saving' }
  }

  private status(key: string, commit: NavCommit): MockIndexStatus {
    return this.statusOf(this.jobs.get(key), commit.sha, Date.now())
  }

  /** Starts a build, or joins the one under way; a commit that is built answers at once. */
  private start(key: string, commit: NavCommit): MockIndexStatus {
    const now = Date.now()
    const current = this.statusOf(this.jobs.get(key), commit.sha, now)
    // A job that is stopping is not joined: the build that follows its cancel is a new one.
    if ((current.state === 'ready' || current.state === 'queued' || current.state === 'building') && !current.cancelling) return current
    const files = commit.tree.filter((e) => languageOf(e.path) && this.blob(e.oid)).length
    this.jobs.set(key, { startedAt: now, durationMs: this.durationMs, files, cancelledAt: null, failure: this.nextFailure })
    this.nextFailure = null
    return this.statusOf(this.jobs.get(key), commit.sha, now)
  }

  private cancel(key: string, commit: NavCommit): MockIndexStatus {
    const job = this.jobs.get(key)
    const now = Date.now()
    const current = this.statusOf(job, commit.sha, now)
    if (job && job.cancelledAt === null && (current.state === 'queued' || current.state === 'building')) job.cancelledAt = now
    return this.statusOf(job, commit.sha, now)
  }

  /** The file of a tree entry, read once; null for one that is gone or too big. */
  private file(entry: { path: string; oid: string }, language: string): Parsed | null {
    const key = `${entry.oid}:${language}`
    if (!this.parsed.has(key)) {
      const blob = this.blob(entry.oid)
      const found = blob ? fileSymbols(entry.path, blob) : null
      let text = blob && found && !found.tooLarge ? blob.toString('utf8') : ''
      if (text.charCodeAt(0) === 0xfeff) text = text.slice(1)
      this.parsed.set(key, found && !found.tooLarge ? { text, lines: text.split('\n'), language, symbols: found.symbols } : null)
    }
    return this.parsed.get(key) ?? null
  }

  /** Where the definitions of a name are, nearest `from` first. */
  private definitions(commit: NavCommit, from: string, name: string, family: string): MockHit[] {
    const hits: Array<MockHit & { near: number }> = []
    for (const entry of commit.tree) {
      const language = languageOf(entry.path)
      const file = language && familyOf(language) === family ? this.file(entry, language) : null
      if (!file?.text.includes(name)) continue
      for (const s of file.symbols) {
        if (s.name !== name || s.kind === 'implementation') continue
        const { snippet, snippetColumn } = snippetOf(file.lines[s.line - 1] ?? '', s.column)
        hits.push({ path: entry.path, line: s.line, column: s.column, endColumn: s.endColumn, kind: s.kind, snippet, snippetColumn, tier: tierOf(entry.path, from), near: commonDirs(entry.path, from) })
      }
    }
    hits.sort((a, b) => TIERS.indexOf(a.tier) - TIERS.indexOf(b.tier) || b.near - a.near || byPath(a.path, b.path) || a.line - b.line)
    return hits.slice(0, MAX_DEFINITIONS).map(({ near: _, ...hit }) => hit)
  }

  /** Where a name is used: the identifiers of that name in the code that are not a definition's name. */
  private references(commit: NavCommit, from: string, name: string, family: string): { hits: MockHit[]; truncated: boolean } {
    const word = new RegExp(`(?<![\\p{L}\\p{N}_$])${escapeRegExp(name)}(?![\\p{L}\\p{N}_$])`, 'gu')
    const hits: MockHit[] = []
    for (const entry of commit.tree) {
      const language = languageOf(entry.path)
      const file = language && familyOf(language) === family ? this.file(entry, language) : null
      if (!file?.text.includes(name)) continue
      file.code ??= blank(file.text, file.language).split('\n')
      file.code.forEach((code, n) => {
        for (const m of code.matchAll(word)) {
          const column = m.index + 1
          const at = (s: MockSymbol) => s.line === n + 1 && s.column === column
          if (file.symbols.some((s) => s.name === name && s.kind !== 'implementation' && at(s))) continue
          const kind = file.symbols.some((s) => s.kind === 'implementation' && at(s)) ? 'implementation' : code[m.index + name.length] === '(' ? 'call' : 'reference'
          const { snippet, snippetColumn } = snippetOf(file.lines[n] ?? '', column)
          hits.push({ path: entry.path, line: n + 1, column, endColumn: column + name.length, kind, snippet, snippetColumn, tier: tierOf(entry.path, from) })
        }
      })
    }
    hits.sort((a, b) => TIERS.indexOf(a.tier) - TIERS.indexOf(b.tier) || byPath(a.path, b.path) || a.line - b.line || a.column - b.column)
    return { hits: hits.slice(0, MAX_REFERENCES), truncated: hits.length > MAX_REFERENCES }
  }

  /** The routes `/pulls/{n}/index`, `/definitions` and `/references`; null for a method they don't have. */
  handle(method: string, sub: 'index' | 'definitions' | 'references', n: number, query: URLSearchParams, body: unknown, pull: NavPull): MockResponse | null {
    const asked = method === 'POST' ? ((body ?? {}) as Record<string, unknown>) : { head: query.get('head'), side: query.get('side') }
    const { head, side } = asked
    if (typeof head !== 'string' || !/^[0-9a-f]{40}$/.test(head)) return failure(400, 'bad_request', 'head must be 40 hexadecimal digits')
    if (side !== 'head' && side !== 'base') return failure(400, 'bad_request', 'side must be head or base')
    const path = query.get('path') ?? ''
    const name = query.get('name') ?? ''
    if (sub !== 'index') {
      if (!path || Buffer.byteLength(path) > 4096 || /[\x00-\x1f\x7f]/.test(path)) return failure(400, 'bad_request', 'invalid path')
      if (!name || Buffer.byteLength(name) > 256 || /[\s\x00-\x1f\x7f]/.test(name)) return failure(400, 'bad_request', 'invalid name')
    }
    if (head !== pull.head) return failure(409, 'stale_head', 'the pull request head has moved')
    if (this.unavailable) return failure(503, 'index_unavailable', 'the index database could not be opened')
    const commit = pull.commits[side]
    const key = `${n}:${head}:${side}`
    if (sub === 'index') {
      if (method === 'POST') return response(200, this.start(key, commit))
      if (method === 'GET') return response(200, this.status(key, commit))
      if (method === 'DELETE') return response(200, this.cancel(key, commit))
      return null
    }
    if (method !== 'GET') return null
    const language = languageOf(path)
    // A file the index has no language for needs no index.
    if (!language) return response(200, { language: null, commit: commit.sha, name, hits: [], truncated: false } satisfies MockNavResult)
    if (this.status(key, commit).state !== 'ready') return failure(409, 'index_missing', 'the commit has not been indexed')
    const found = sub === 'definitions' ? { hits: this.definitions(commit, path, name, familyOf(language)), truncated: false } : this.references(commit, path, name, familyOf(language))
    return response(200, { language, commit: commit.sha, name, ...found } satisfies MockNavResult)
  }
}
