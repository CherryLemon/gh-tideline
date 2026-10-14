// The name under a position in a line of code, which is what Ctrl+click and Ctrl+B look up.

/** An identifier in a line; the columns are Monaco's (1-based UTF-16, the end exclusive). */
export interface Identifier {
  name: string
  line: number
  /** Where the identifier starts, and ends: for a Rust raw identifier (`r#type`), with its `r#`. */
  column: number
  endColumn: number
  /** Where the name starts: after the `r#` of a raw identifier, as the index has it; else `column`. */
  nameColumn: number
}

/** What a token is, as far as a lookup cares: names in comments, strings and regular expressions are not code. */
export type TokenKind = 'code' | 'comment' | 'string' | 'regex'

/** The part of a text model that is read: Monaco's `ITextModel` is one. */
export interface IdentifierSource {
  getLanguageId(): string
  getLineContent(line: number): string
  /** The kind of the token that holds the character at the 1-based `column`; null (or absent) when the tokenizer can't say. */
  tokenKindAt?(line: number, column: number): TokenKind | null
}

/** Letters, digits, marks and connectors (`_`) of any script: Unicode's identifier characters. */
const ID_CHARS = String.raw`\p{L}\p{Nl}\p{Mn}\p{Mc}\p{Nd}\p{Pc}`
const WORD = new RegExp(`[${ID_CHARS}]+`, 'gu')
/** JavaScript, TypeScript and Java also allow `$` in a name. */
const WORD_WITH_DOLLAR = new RegExp(`[${ID_CHARS}$]+`, 'gu')
const DOLLAR_LANGUAGES = new Set(['javascript', 'typescript', 'java'])
const WORD_AT_START = new RegExp(`^[${ID_CHARS}]+`, 'u')
const WORD_AT_END = new RegExp(`[${ID_CHARS}]$`, 'u')

const words = (list: string) => new Set(list.trim().split(/\s+/))

const ECMASCRIPT_KEYWORDS = words(`
  await break case catch class const continue debugger default delete do else enum export extends false finally for function if implements
  import in instanceof interface let new null package private protected public return static super switch this throw true try typeof var
  void while with yield`)
const C_KEYWORDS = `
  auto break case char const continue default do double else enum extern float for goto if inline int long register return short signed
  sizeof static struct switch typedef union unsigned void volatile while`

/**
 * The words each language (by Monaco's id) reserves, that are never a name: a lookup of `return` finds nothing, or, if some class
 * has a method of that name, the wrong thing. Words that are keywords only where they stand (`type`, `from`, `get`, `match`, `union`,
 * `operator`, which `operator==` is looked up by) are names elsewhere, and are left out.
 */
const KEYWORDS: Record<string, ReadonlySet<string> | undefined> = {
  javascript: ECMASCRIPT_KEYWORDS,
  typescript: ECMASCRIPT_KEYWORDS,
  rust: words(`
    as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self
    Self static struct super trait true type unsafe use where while`),
  go: words(`
    break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select
    struct switch type var`),
  python: words(`
    False None True and as assert async await break class continue def del elif else except finally for from global if import in is lambda
    nonlocal not or pass raise return try while with yield`),
  java: words(`
    abstract assert boolean break byte case catch char class continue default do double else enum extends false final finally float for if
    implements import instanceof int interface long native new null package private protected public return short static strictfp super
    switch synchronized this throw throws transient true try void volatile while`),
  c: words(`${C_KEYWORDS} restrict`),
  cpp: words(`
    ${C_KEYWORDS} alignas alignof and and_eq asm bitand bitor bool catch char8_t char16_t char32_t class compl concept const_cast consteval
    constexpr constinit co_await co_return co_yield decltype delete dynamic_cast explicit export false friend mutable namespace new noexcept
    not not_eq nullptr or or_eq private protected public reinterpret_cast requires static_assert static_cast template this thread_local
    throw true try typeid typename using virtual wchar_t xor xor_eq`),
}
/** JavaScript and TypeScript let a property be called what a keyword is: `map.delete(key)`. */
const PROPERTY_LANGUAGES = new Set(['javascript', 'typescript'])

/** How far from the position a name is looked for; a longer one is no name worth a lookup (a minified line, a blob of letters). */
const REACH = 300
/** The most the server takes in a name. */
const MAX_NAME_BYTES = 256

const encoder = new TextEncoder()

/** A stretch of a line: 0-based, the end exclusive. */
interface Span {
  start: number
  end: number
}

/**
 * The identifier at `position`. Like the word at a caret, it may touch either end of the identifier. Null when there is
 * none: no word, a number, a keyword, a name in a comment or a string, or an absurdly long word. A Rust raw identifier
 * (`r#type`) is the name without the `r#`, and covers it.
 */
export function identifierAt(source: IdentifierSource, position: { lineNumber: number; column: number }): Identifier | null {
  const text = source.getLineContent(position.lineNumber)
  const language = source.getLanguageId()
  const word = wordAt(text, position.column - 1, DOLLAR_LANGUAGES.has(language) ? WORD_WITH_DOLLAR : WORD)
  if (!word) return null
  const raw = language === 'rust' ? rawIdentifier(text, word) : null
  const covered = raw?.covered ?? word
  const name = text.slice(raw?.name.start ?? word.start, raw?.name.end ?? word.end)
  // `3.14`, `0xff`, `1_000`: the words of a number begin with a digit.
  if (/^\p{Nd}/u.test(name) || encoder.encode(name).length > MAX_NAME_BYTES) return null
  // A raw identifier is the way to name what the keyword is, and a property of an object can be called one.
  if (!raw && KEYWORDS[language]?.has(name) && !(PROPERTY_LANGUAGES.has(language) && afterDot(text, word.start))) return null
  const kind = source.tokenKindAt?.(position.lineNumber, (raw?.name.start ?? word.start) + 1) ?? 'code'
  if (kind !== 'code') return null
  return { name, line: position.lineNumber, column: covered.start + 1, endColumn: covered.end + 1, nameColumn: (raw?.name.start ?? word.start) + 1 }
}

/** Whether the offset follows a single dot (`a.b`, `a?.b`): not the three of a spread, in `...this`. */
function afterDot(text: string, at: number): boolean {
  return text[at - 1] === '.' && text[at - 2] !== '.'
}

/** The word that holds the offset `at` or ends there; null if there is none or it runs past what is looked at. */
function wordAt(text: string, at: number, words: RegExp): Span | null {
  if (at < 0 || at > text.length) return null
  const from = Math.max(0, at - REACH)
  const to = Math.min(text.length, at + REACH)
  for (const m of text.slice(from, to).matchAll(words)) {
    const start = from + m.index
    const end = start + m[0].length
    if (at < start || at > end) continue
    // A word that runs into the edge of what is looked at may be longer than it seems.
    if ((start === from && from > 0) || (end === to && to < text.length)) return null
    return { start, end }
  }
  return null
}

/** `r#type`, when `word` is the `r` or the `type` of one: the stretch it covers and the name in it. */
function rawIdentifier(text: string, word: Span): { covered: Span; name: Span } | null {
  if (text.slice(word.start, word.end) === 'r' && text[word.end] === '#') {
    const name = WORD_AT_START.exec(text.slice(word.end + 1))
    if (name) {
      const end = word.end + 1 + name[0].length
      return { covered: { start: word.start, end }, name: { start: word.end + 1, end } }
    }
  }
  if (word.start >= 2 && text.slice(word.start - 2, word.start) === 'r#' && !WORD_AT_END.test(text.slice(0, word.start - 2))) {
    return { covered: { start: word.start - 2, end: word.end }, name: word }
  }
  return null
}
