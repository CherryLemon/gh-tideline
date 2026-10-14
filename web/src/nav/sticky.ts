import type { SymbolInfo } from '../api'
import { enclosingScopes } from './symbols'

/** The most rows the header has. */
export const MAX_ROWS = 4

/** Definitions that hold a value rather than code: a long constant or field is not a place worth pinning. */
const DATA_KINDS = new Set(['constant', 'field', 'variable', 'property'])

/** Lines of an editor, as `getVisibleRanges` gives them. */
export interface LineRange {
  startLineNumber: number
  endLineNumber: number
}

/**
 * Where the lines of an editor are, in pixels from the top of its content. A line it hides is drawn at the end of the line shown
 * before it, or at the first line shown if none is: `nextBar` goes by that. This is how monaco-editor 0.57 does it, in
 * `convertModelPositionToViewPosition` (vs/editor/common/viewModel/viewModelLines.js ~621), which `getTopForLineNumber` and
 * `getBottomForLineNumber` (vs/editor/browser/widget/codeEditor/codeEditorWidget.js ~441, ~461) are made of: check it on an upgrade.
 */
export interface LineEdges {
  /** The top of `line`, under the view zones (bars, cards) above it; of its first row, if it wraps. */
  top(line: number): number
  /** The top of the view zones above `line`: the bottom of the line drawn before it, or the padding at the top of the content. */
  zonesTop(line: number): number
  /** The bottom of `line`, of its last row if it wraps. */
  bottom(line: number): number
}

/** A stretch of the content, in pixels from its top. */
export interface Span {
  top: number
  bottom: number
}

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
 * The scopes to pin over the code of an editor, outermost first. The rows cover the first lines in view, so what they say is what
 * the first line below them is in: `lineAt(n)` is that line when `n` rows are pinned (for 0, the first line in view), or undefined
 * where the view ends. More rows move it down, and a scope can end under them, so the fewest rows `n` whose scopes need no more than
 * `n` are used: a scope is pinned while some of its body shows below the rows, not when only its last line is under them, and one
 * whose name is under them still gets its row.
 * Those scopes can be fewer than `n`, and then the rows leave an earlier line under them than the one they were worked out for: a
 * scope named on that line or after it is not drawn, as its row would show that line twice or one that is not reached yet. That
 * moves the line up again, so it goes on until no such scope is left. What is drawn is then in the line under the rows, with its
 * name above it, but the line can be in more scopes than that (it can be the last line of one that has no row).
 * There are at most `maxLines` rows, and no more than the view has lines below; with less room the innermost scopes are kept.
 */
export function stickyLines(symbols: readonly SymbolInfo[], lineAt: (rows: number) => number | undefined, maxLines = MAX_ROWS): SymbolInfo[] {
  const lines: number[] = []
  for (let rows = 0; rows <= maxLines; rows++) {
    const line = lineAt(rows)
    if (line === undefined) break
    lines.push(line)
  }
  const room = lines.length - 1
  for (let rows = 0; rows <= room; rows++) {
    let scopes = scopesAround(symbols, lines[rows], room)
    if (scopes.length > rows) continue
    for (;;) {
      const under = lines[scopes.length]
      const named = scopes.filter((s) => s.line < under)
      if (named.length === scopes.length) return named
      scopes = named
    }
  }
  return []
}

/** The first line of `ranges` (the lines in view) whose bottom is below `y`, an offset from the top of the content; undefined if none is. */
export function lineBelow(ranges: readonly LineRange[], bottom: (line: number) => number, y: number): number | undefined {
  for (const range of ranges) {
    if (bottom(range.endLineNumber) <= y) continue
    let low = range.startLineNumber
    let high = range.endLineNumber
    while (low < high) {
      const mid = (low + high) >> 1
      if (bottom(mid) > y) high = mid
      else low = mid + 1
    }
    return low
  }
  return undefined
}

/**
 * The first bar that is not wholly above the view: the view zones over the first line of a range of `ranges` (the lines in view)
 * that comes after lines the editor hides, or over the first line of the content. The diff editor puts a bar where it hides the
 * unchanged lines, between its changes and above the first one, and a card at the top of the content is a zone like it.
 * A zone between two lines the editor shows (a card, a filler, the deleted lines of an inline diff) is no bar.
 * A card under the last line before a bar is taken for a part of it, and the rows stay above both. That is not met in use, as
 * the lines a card hangs on and the one after are kept in view (`ZoneHost.reveal`): it takes dragging the edge of the bar up to
 * the card's line.
 */
export function nextBar(ranges: readonly LineRange[], edges: LineEdges, scrollTop: number, paddingTop: number): Span | undefined {
  for (const { startLineNumber: line } of ranges) {
    const top = edges.zonesTop(line)
    const bottom = edges.top(line)
    if (bottom <= top || bottom <= scrollTop) continue
    // No line is above this one, or the line before it is hidden, which puts it where the one before that is. They have the same
    // bottom: not the same top, if that line wraps, as a hidden line is drawn at the last row of the line before it. Not for line 2:
    // the editor takes line 0 for line 1, so those would always have the same bottom, and a card under line 1 would be a bar.
    if (top <= paddingTop || (line >= 3 && edges.bottom(line - 1) === edges.bottom(line - 2))) return { top, bottom }
  }
  return undefined
}

/**
 * How many rows of `rowHeight` fit between the top of the view and `bar`, with `gap` pixels left under them (the rule under the
 * rows, and what is drawn of the bar above its zone): rows over a bar hide its controls and the unchanged lines it counts.
 * All `MAX_ROWS` if there is no bar; none once the bar reaches the top.
 */
export function rowsBeforeBar(bar: Span | undefined, scrollTop: number, rowHeight: number, gap: number): number {
  if (!bar) return MAX_ROWS
  return Math.max(0, Math.min(MAX_ROWS, Math.floor((bar.top - scrollTop - gap) / rowHeight)))
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
