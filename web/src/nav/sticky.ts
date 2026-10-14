import type { SymbolInfo } from '../api'
import { enclosingScopes } from './symbols'

/** Definitions that hold a value rather than code: a long constant or field is not a place worth pinning. */
const DATA_KINDS = new Set(['constant', 'field', 'variable', 'property'])

/**
 * The scopes to pin at the top of an editor whose first visible line is `top`: those whose body holds it, outermost
 * first, at most `maxLines` of them (the innermost when there are more). A row shows the line of the scope's name (not the
 * decorators above it), and a scope whose name is on screen already has no row.
 */
export function stickyLines(symbols: readonly SymbolInfo[], top: number, maxLines = 4): SymbolInfo[] {
  const rows: SymbolInfo[] = []
  for (const s of enclosingScopes(symbols, top)) {
    if (DATA_KINDS.has(s.kind) || s.line >= top) continue
    // Two scopes named on one line show it once, as the outer one.
    if (rows.length > 0 && rows[rows.length - 1].line === s.line) continue
    rows.push(s)
  }
  return rows.slice(Math.max(0, rows.length - maxLines))
}
