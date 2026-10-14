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
 * none: no word, a number, a name in a comment or a string, or an absurdly long word. A Rust raw identifier (`r#type`)
 * is the name without the `r#`, and covers it.
 */
export function identifierAt(source: IdentifierSource, position: { lineNumber: number; column: number }): Identifier | null {
  const text = source.getLineContent(position.lineNumber)
  const word = wordAt(text, position.column - 1, DOLLAR_LANGUAGES.has(source.getLanguageId()) ? WORD_WITH_DOLLAR : WORD)
  if (!word) return null
  const raw = source.getLanguageId() === 'rust' ? rawIdentifier(text, word) : null
  const covered = raw?.covered ?? word
  const name = text.slice(raw?.name.start ?? word.start, raw?.name.end ?? word.end)
  // `3.14`, `0xff`, `1_000`: the words of a number begin with a digit.
  if (/^\p{Nd}/u.test(name) || encoder.encode(name).length > MAX_NAME_BYTES) return null
  const kind = source.tokenKindAt?.(position.lineNumber, (raw?.name.start ?? word.start) + 1) ?? 'code'
  if (kind !== 'code') return null
  return { name, line: position.lineNumber, column: covered.start + 1, endColumn: covered.end + 1, nameColumn: (raw?.name.start ?? word.start) + 1 }
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
