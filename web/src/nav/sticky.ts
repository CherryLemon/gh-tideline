import type { SymbolInfo } from '../api'
import { enclosingScopes } from './symbols'

/** Definitions that hold a value rather than code: a long constant or field is not a place worth pinning. */
const DATA_KINDS = new Set(['constant', 'field', 'variable', 'property'])

/**
 * The scopes around `line`, outermost first, at most `maxLines` of them (the innermost when there are more): those whose body
 * holds it. A row shows the line of the scope's name (not the decorators above it), and a scope whose name is not above
 * `line` has no row.
 */
function scopesAround(symbols: readonly SymbolInfo[], line: number, maxLines: number): SymbolInfo[] {
  const rows: SymbolInfo[] = []
  for (const s of enclosingScopes(symbols, line)) {
    if (DATA_KINDS.has(s.kind) || s.line >= line) continue
    // Two scopes named on one line show it once, as the outer one.
    if (rows.length > 0 && rows[rows.length - 1].line === s.line) continue
    rows.push(s)
  }
  return rows.slice(Math.max(0, rows.length - maxLines))
}

/**
 * The scopes to pin over the code of an editor, outermost first. The rows cover the first lines in view, so what they say is
 * what the first line below them is in: `lineAt(n)` is that line when `n` rows are pinned (for 0, the first line in view), or
 * undefined where the view ends. More rows move it down, and a scope can end under them, so the fewest rows whose scopes
 * need no more than that are used: a scope is pinned while some of its body shows below the rows, not when only its last
 * line is under them, and one whose name is under them still gets its row.
 */
export function stickyLines(symbols: readonly SymbolInfo[], lineAt: (rows: number) => number | undefined, maxLines = 4): SymbolInfo[] {
  for (let rows = 0; rows <= maxLines; rows++) {
    const line = lineAt(rows)
    const scopes = line === undefined ? [] : scopesAround(symbols, line, maxLines)
    if (scopes.length <= rows) return scopes
  }
  return []
}

/** Whether two lists of lines are the same. */
export function sameLines(a: readonly number[], b: readonly number[]): boolean {
  return a.length === b.length && a.every((line, i) => line === b[i])
}

/**
 * Where to scroll to bring a caret out from under the `covered` pixels at the top of the view: `caretTop` and `scrollTop` are
 * offsets from the top of the content. Null when the caret is not under them, or the view cannot go any higher.
 */
export function scrollToUncover(caretTop: number, scrollTop: number, covered: number): number | null {
  if (caretTop < scrollTop || caretTop >= scrollTop + covered) return null
  const next = Math.max(0, caretTop - covered)
  return next < scrollTop ? next : null
}
