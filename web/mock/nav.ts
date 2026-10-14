// The mock of GET /blobs/{oid}/symbols. A few regular expressions per language stand in for the server's tree-sitter tags: at
// most one definition per line, with the kinds the server reports, and an extent found by matching braces (by indentation in
// Python). Good enough for the fixtures; it does not parse anything.

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
