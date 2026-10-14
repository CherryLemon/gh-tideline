import { describe, expect, it } from 'vitest'
import type { SymbolInfo } from '../api'
import { type LineEdges, type LineRange, lineBelow, nextBar, rowsBeforeBar, sameLines, scrollToUncover, stickyLines } from './sticky'

function sym(name: string, kind: string, startLine: number, endLine: number, extra: Partial<SymbolInfo> = {}): SymbolInfo {
  return { name, kind, line: startLine, column: 1, endColumn: 1 + name.length, startLine, endLine, ...extra }
}

const FILE = [
  sym('Registry', 'class', 3, 60),
  sym('add', 'method', 10, 20),
  sym('helper', 'function', 12, 14),
  sym('find', 'method', 22, 40),
  sym('main', 'function', 70, 90),
]

/** The scopes around one line, however many rows they take: the first line below the rows is the same for any number of them. */
const rows = (list: SymbolInfo[], top: number, max?: number) => stickyLines(list, () => top, max).map((s) => s.name)

describe('stickyLines: the scopes around a line', () => {
  it('pins the scopes around the first visible line, outermost first', () => {
    expect(rows(FILE, 4)).toEqual(['Registry'])
    expect(rows(FILE, 11)).toEqual(['Registry', 'add'])
    expect(rows(FILE, 13)).toEqual(['Registry', 'add', 'helper'])
    expect(rows(FILE, 30)).toEqual(['Registry', 'find'])
  })

  it('pins nothing above the first scope, between scopes, or after the last', () => {
    expect(rows(FILE, 1)).toEqual([])
    expect(rows(FILE, 3)).toEqual([])
    expect(rows(FILE, 65)).toEqual([])
    expect(rows(FILE, 91)).toEqual([])
  })

  it('does not pin a scope while its first line is the one at the top', () => {
    expect(rows(FILE, 10)).toEqual(['Registry'])
    expect(rows(FILE, 22)).toEqual(['Registry'])
  })

  it('stops pinning a scope after its last line', () => {
    expect(rows(FILE, 20)).toEqual(['Registry', 'add'])
    expect(rows(FILE, 21)).toEqual(['Registry'])
    expect(rows(FILE, 60)).toEqual(['Registry'])
    expect(rows(FILE, 61)).toEqual([])
  })

  it('keeps the innermost four when more scopes surround the line', () => {
    const deep = ['a', 'b', 'c', 'd', 'e', 'f'].map((name, i) => sym(name, 'function', 1 + i, 100 - i))
    expect(rows(deep, 50)).toEqual(['c', 'd', 'e', 'f'])
    expect(rows(deep, 50, 2)).toEqual(['e', 'f'])
    expect(rows(deep, 50, 10)).toEqual(['a', 'b', 'c', 'd', 'e', 'f'])
    expect(rows(deep, 50, 0)).toEqual([])
  })

  it('does not pin a constant, field, variable or property', () => {
    const list = [
      sym('Config', 'class', 1, 50),
      sym('TABLE', 'constant', 5, 30),
      sym('size', 'field', 6, 9),
      sym('tmp', 'variable', 7, 9),
      sym('width', 'property', 8, 9),
      sym('run', 'method', 12, 40),
    ]
    expect(rows(list, 8)).toEqual(['Config'])
    expect(rows(list, 20)).toEqual(['Config', 'run'])
  })

  it('shows the line of the name, not the first line of a definition that starts with a decorator', () => {
    const list = [sym('Service', 'class', 4, 30, { line: 6 }), sym('handle', 'method', 9, 20, { line: 11 })]
    // Line 5 is a decorator line of Service: its name, on line 6, is not in view yet.
    expect(rows(list, 5)).toEqual([])
    expect(rows(list, 6)).toEqual([])
    expect(rows(list, 7)).toEqual(['Service'])
    // The first lines of `handle`, its decorators, are inside Service and above its name.
    expect(rows(list, 10)).toEqual(['Service'])
    expect(rows(list, 12)).toEqual(['Service', 'handle'])
  })

  it('shows a line once when two scopes are named on it, as the outer one', () => {
    const list = [sym('outer', 'function', 2, 30), sym('inner', 'function', 2, 20, { column: 20, endColumn: 25 })]
    expect(rows(list, 10)).toEqual(['outer'])
  })

  it('does not depend on the order of the input', () => {
    expect(rows([...FILE].reverse(), 13)).toEqual(['Registry', 'add', 'helper'])
  })

  it('pins nothing for an empty list', () => {
    expect(rows([], 10)).toEqual([])
  })
})

// A class with a method that holds a nested function, as in the mock repository's registry.ts.
const NESTED = [sym('Registry', 'class', 20, 74), sym('list', 'method', 43, 56), sym('byWeight', 'function', 44, 52), sym('render', 'method', 58, 61)]

/** Lines of one height, none hidden: with `rows` rows over the top, the first line below them is `rows` lines under `top`. */
const below = (top: number) => (rows: number) => top + rows
const pinned = (list: SymbolInfo[], top: number, max?: number) => stickyLines(list, below(top), max).map((s) => s.name)

describe('stickyLines: the rows cover the first lines', () => {
  it('pins a scope whose name is under the rows, when the body goes on below them', () => {
    // byWeight starts on line 44, the first line in view: it is under the row of Registry, and its body is in view below the rows.
    expect(pinned(NESTED, 44)).toEqual(['Registry', 'list', 'byWeight'])
    expect(pinned(NESTED, 43)).toEqual(['Registry', 'list', 'byWeight'])
    expect(pinned(NESTED, 45)).toEqual(['Registry', 'list', 'byWeight'])
  })

  it('does not pin a scope that ends under the rows it needs', () => {
    // At 50 the three rows byWeight needs would cover the lines 50 to 52, the last of byWeight: the line below them, 53, is in
    // list only. The two rows drawn leave 52 right under them, and byWeight has no row for it.
    expect(pinned(NESTED, 49)).toEqual(['Registry', 'list', 'byWeight'])
    expect(pinned(NESTED, 50)).toEqual(['Registry', 'list'])
    expect(pinned(NESTED, 51)).toEqual(['Registry', 'list'])
    expect(pinned(NESTED, 52)).toEqual(['Registry', 'list'])
  })

  it('pins fewer rows as the scopes end under them, down to none', () => {
    // At 55 the rows (Registry, list) would leave 57 as the first line below them, which is in Registry only.
    expect(pinned(NESTED, 54)).toEqual(['Registry', 'list'])
    expect(pinned(NESTED, 55)).toEqual(['Registry'])
    expect(pinned(NESTED, 73)).toEqual(['Registry'])
    expect(pinned(NESTED, 74)).toEqual([])
  })

  it('pins the scopes of the line below the fewest rows that have room for them', () => {
    // The scopes around a line by their definition, outermost first: the body holds the line, and the name is above it.
    const around = (line: number) => NESTED.filter((s) => s.startLine <= line && line <= s.endLine && s.line < line).map((s) => s.name)
    for (let top = 1; top <= 80; top++) {
      // How many scopes the line below 0, 1, 2... rows has: the first number of rows that is no less than that many is the one.
      const needed = Array.from({ length: 5 }, (_, rows) => around(top + rows).length)
      const fewest = needed.findIndex((count, rows) => count <= rows)
      expect(fewest, `top ${top}`).toBeGreaterThanOrEqual(0)
      expect(stickyLines(NESTED, below(top)).map((s) => s.name), `top ${top}`).toEqual(around(top + fewest))
    }
  })

  it('pins nothing in a view without a line', () => {
    expect(stickyLines(NESTED, () => undefined)).toEqual([])
  })

  it('pins fewer rows, the innermost scopes, where the view has no line below more of them', () => {
    // A view of `count` lines from 49: the rows it has room for leave a line below them.
    const view = (count: number) => (rows: number) => (rows < count ? 49 + rows : undefined)
    const names = (count: number) => stickyLines(NESTED, view(count)).map((s) => s.name)
    expect(names(5)).toEqual(['Registry', 'list', 'byWeight'])
    expect(names(4)).toEqual(['Registry', 'list', 'byWeight'])
    expect(names(3)).toEqual(['list', 'byWeight'])
    expect(names(2)).toEqual(['byWeight'])
    expect(names(1)).toEqual([])
    expect(names(0)).toEqual([])
  })

  it('counts rows by the lines the editor shows, not by line numbers', () => {
    const list = [sym('Big', 'class', 1, 200), sym('f', 'method', 10, 120)]
    // The lines between 20 and 100 are folded away: the line two rows below 19 is 101.
    const shown = [18, 19, 20, 101, 102, 103]
    const lineAt = (rows: number) => shown[shown.indexOf(19) + rows]
    expect(stickyLines(list, lineAt).map((s) => s.name)).toEqual(['Big', 'f'])
    // Under 120 (end of f) nothing but Big is left in view.
    const tail = [117, 118, 119, 120, 121, 122, 123]
    expect(stickyLines(list, (rows) => tail[tail.indexOf(118) + rows]).map((s) => s.name)).toEqual(['Big', 'f'])
    expect(stickyLines(list, (rows) => tail[tail.indexOf(119) + rows]).map((s) => s.name)).toEqual(['Big'])
  })

  it('keeps the innermost rows when more scopes surround the line below', () => {
    const deep = ['a', 'b', 'c', 'd', 'e', 'f'].map((name, i) => sym(name, 'function', 1 + i, 100 - i))
    expect(pinned(deep, 50)).toEqual(['c', 'd', 'e', 'f'])
    expect(pinned(deep, 50, 2)).toEqual(['e', 'f'])
    expect(pinned(deep, 50, 0)).toEqual([])
  })
})

/**
 * The lines of an editor as Monaco lays them out, 20px high with `padding` above the first: a line it hides is drawn where the line
 * before it is (where the first line shown is, if none is before it), and `zones[k]` pixels of view zones sit after line `k` (0 is
 * the top of the content), or after the line shown before it when `k` is hidden.
 */
function layout(count: number, hidden: [number, number][], zones: Record<number, number> = {}, padding = 4) {
  const isHidden = (line: number) => hidden.some(([from, to]) => from <= line && line <= to)
  const shownUpTo = (line: number) => Array.from({ length: line }, (_, i) => i + 1).filter((n) => !isHidden(n)).length
  const slot = (line: number) => Math.max(shownUpTo(line), 1)
  /** The pixels of the zones after the first `drawn` lines shown, or fewer. */
  const zonesUpTo = (drawn: number) => Object.entries(zones).reduce((sum, [line, height]) => (shownUpTo(Number(line)) <= drawn ? sum + height : sum), 0)
  const edges: LineEdges = {
    top: (line) => padding + 20 * (slot(line) - 1) + zonesUpTo(slot(line) - 1),
    zonesTop: (line) => padding + 20 * (slot(line) - 1) + zonesUpTo(slot(line) - 2),
    bottom: (line) => edges.top(line) + 20,
  }
  /** What `getVisibleRanges` gives: the lines shown that are wholly in a view of `height` pixels at `scrollTop`, split where lines are hidden. */
  const visible = (scrollTop: number, height: number): LineRange[] => {
    const ranges: LineRange[] = []
    for (let line = 1; line <= count; line++) {
      if (isHidden(line) || edges.top(line) < scrollTop || edges.bottom(line) > scrollTop + height) continue
      const last = ranges[ranges.length - 1]
      if (last && last.endLineNumber === line - 1) last.endLineNumber = line
      else ranges.push({ startLineNumber: line, endLineNumber: line })
    }
    return ranges
  }
  return { edges, visible }
}

// worker.ts of the mock repository: the diff hides the lines above its first change, 1 to 57, under a bar at the top, and 66 to 73
// under one in the middle. The bars are 24px, the second sits after line 65: it is from 188 to 212, and line 74 is under it.
const worker = layout(80, [[1, 57], [66, 73]], { 0: 24, 65: 24 })

describe('lineBelow: the line a pixel offset is in, among the lines in view', () => {
  const ranges = [{ startLineNumber: 58, endLineNumber: 65 }, { startLineNumber: 74, endLineNumber: 80 }]
  const at = (y: number) => lineBelow(ranges, worker.edges.bottom, y)

  it('finds the line that reaches below the offset', () => {
    expect(at(0)).toBe(58)
    expect(at(47)).toBe(58)
    expect(at(48)).toBe(59)
    expect(at(187)).toBe(65)
  })

  it('skips the hidden lines and the bar that stands for them', () => {
    expect(at(188)).toBe(74)
    expect(at(200)).toBe(74)
    expect(at(231)).toBe(74)
    expect(at(232)).toBe(75)
  })

  it('finds nothing below the last line in view, or where no line is in view', () => {
    expect(lineBelow([{ startLineNumber: 74, endLineNumber: 76 }], worker.edges.bottom, 292)).toBeUndefined()
    expect(lineBelow([], worker.edges.bottom, 0)).toBeUndefined()
  })
})

describe('nextBar: the bar of hidden lines that rows would cover', () => {
  const bar = (scrollTop: number, height = 144, edges = worker) => nextBar(edges.visible(scrollTop, height), edges.edges, scrollTop, 4)

  it('finds the bar over the first change while any of it is in view, and then not', () => {
    expect(bar(0)).toEqual({ top: 4, bottom: 28 })
    expect(bar(27)).toEqual({ top: 4, bottom: 28 })
    expect(bar(28)).toBeUndefined()
    expect(bar(40)).toBeUndefined()
  })

  it('finds a bar between two stretches of lines in view', () => {
    expect(bar(100)).toEqual({ top: 188, bottom: 212 })
    expect(bar(158)).toEqual({ top: 188, bottom: 212 })
    // Only the bar and not the one at the top, which has been scrolled out of view.
    expect(bar(28, 300)).toEqual({ top: 188, bottom: 212 })
  })

  it('finds a bar over the first line in view, when the line before it is partly scrolled off', () => {
    // Line 65, from 168 to 188, is out of the range of the lines in view: only the hidden lines are before 74.
    expect(worker.visible(176, 144)[0]).toEqual({ startLineNumber: 74, endLineNumber: 78 })
    expect(bar(176)).toEqual({ top: 188, bottom: 212 })
    expect(bar(188)).toEqual({ top: 188, bottom: 212 })
    expect(bar(200)).toEqual({ top: 188, bottom: 212 })
    expect(bar(211)).toEqual({ top: 188, bottom: 212 })
    expect(bar(212)).toBeUndefined()
  })

  it('goes on to the next bar when one has scrolled out of view', () => {
    const two = layout(100, [[10, 19], [40, 49]], { 9: 24, 39: 24 })
    // The first bar is from 184 to 208 and the second from 608 to 632.
    expect(two.edges.zonesTop(20)).toBe(184)
    expect(two.edges.top(50)).toBe(632)
    expect(bar(150, 400, two)).toEqual({ top: 184, bottom: 208 })
    expect(bar(300, 400, two)).toEqual({ top: 608, bottom: 632 })
  })

  it('does not take a card, a filler or deleted lines between two lines in view for a bar', () => {
    const cards = layout(100, [], { 50: 80, 51: 20 })
    for (let scrollTop = 0; scrollTop <= 2000; scrollTop += 5) expect(bar(scrollTop, 200, cards), `scroll ${scrollTop}`).toBeUndefined()
  })

  it('takes a card at the top of the content for one, as nothing can be scrolled above it', () => {
    const card = layout(100, [], { 0: 80 })
    expect(bar(0, 200, card)).toEqual({ top: 4, bottom: 84 })
    expect(bar(83, 200, card)).toEqual({ top: 4, bottom: 84 })
    expect(bar(84, 200, card)).toBeUndefined()
  })

  it('finds none without lines in view, or where no lines are hidden and nothing is above the first', () => {
    expect(nextBar([], worker.edges, 0, 4)).toBeUndefined()
    expect(bar(0, 200, layout(100, []))).toBeUndefined()
    expect(bar(0, 200, layout(100, [[1, 5]]))).toBeUndefined()
  })
})

describe('rowsBeforeBar: the room above a bar', () => {
  const bar = { top: 188, bottom: 212 }

  it('gives all the rows where no bar is in the way', () => {
    expect(rowsBeforeBar(undefined, 100, 20, 1)).toBe(4)
    expect(rowsBeforeBar(bar, 100, 20, 1)).toBe(4)
  })

  it('fits the rows, and the rule under them, between the top of the view and the bar', () => {
    expect(rowsBeforeBar(bar, 127, 20, 1)).toBe(3)
    expect(rowsBeforeBar(bar, 128, 20, 1)).toBe(2)
    expect(rowsBeforeBar(bar, 148, 20, 1)).toBe(1)
    expect(rowsBeforeBar(bar, 167, 20, 1)).toBe(1)
    expect(rowsBeforeBar(bar, 168, 20, 1)).toBe(0)
  })

  it('leaves the room asked for under the rows', () => {
    expect(rowsBeforeBar(bar, 127, 20, 7)).toBe(2)
    expect(rowsBeforeBar(bar, 121, 20, 7)).toBe(3)
    expect(rowsBeforeBar(bar, 161, 20, 7)).toBe(1)
    expect(rowsBeforeBar(bar, 162, 20, 7)).toBe(0)
    expect(rowsBeforeBar(bar, 100, 20, 0)).toBe(4)
  })

  it('gives none once the bar reaches the top of the view', () => {
    expect(rowsBeforeBar(bar, 188, 20, 1)).toBe(0)
    expect(rowsBeforeBar(bar, 200, 20, 1)).toBe(0)
  })
})

describe('the rows over the lines of an editor that hides some', () => {
  const symbols = [sym('Worker', 'class', 10, 79), sym('drain', 'method', 22, 70), sym('warm', 'method', 72, 74)]
  /** The 1px rule under the rows, and the 6px that Monaco draws of a bar above its zone. */
  const gap = 7
  /** The rows for a view of `height` pixels at `scrollTop`, as the editor works them out, and the room above the bar. */
  const header = (scrollTop: number, height = 144) => {
    const ranges = worker.visible(scrollTop, height)
    const maxRows = rowsBeforeBar(nextBar(ranges, worker.edges, scrollTop, 4), scrollTop, 20, gap)
    return stickyLines(symbols, (rows) => lineBelow(ranges, worker.edges.bottom, scrollTop + rows * 20), maxRows).map((s) => s.name)
  }

  it('leaves the bar in the middle uncovered at any scroll position', () => {
    for (let scrollTop = 0; scrollTop <= 220; scrollTop++) {
      const rows = header(scrollTop).length
      const bar = nextBar(worker.visible(scrollTop, 144), worker.edges, scrollTop, 4)
      if (bar && rows > 0) expect(rows * 20 + gap, `scroll ${scrollTop}`).toBeLessThanOrEqual(bar.top - scrollTop)
    }
  })

  it('has fewer rows as the bar comes up to the top, then none, and comes whole when it is gone', () => {
    expect(header(60)).toEqual(['Worker', 'drain'])
    expect(header(141)).toEqual(['Worker', 'drain'])
    expect(header(142)).toEqual(['drain'])
    expect(header(161)).toEqual(['drain'])
    expect(header(162)).toEqual([])
    expect(header(198)).toEqual([])
    expect(header(212)).toEqual(['Worker'])
  })
})

describe('sameLines', () => {
  it('compares lines in order', () => {
    expect(sameLines([], [])).toBe(true)
    expect(sameLines([3, 9], [3, 9])).toBe(true)
    expect(sameLines([3, 9], [9, 3])).toBe(false)
    expect(sameLines([3, 9], [3])).toBe(false)
    expect(sameLines([3], [3, 9])).toBe(false)
  })
})

describe('scrollToUncover', () => {
  it('moves a caret under the rows to just below them', () => {
    // A caret on the second line of the view, under three 20px rows: the view goes up until the caret has the rows above it.
    expect(scrollToUncover(1020, 1000, 60)).toBe(960)
    expect(scrollToUncover(1000, 1000, 60)).toBe(940)
    expect(scrollToUncover(1059, 1000, 60)).toBe(999)
  })

  it('leaves a caret outside the rows alone', () => {
    expect(scrollToUncover(1060, 1000, 60)).toBeNull()
    expect(scrollToUncover(1400, 1000, 60)).toBeNull()
    expect(scrollToUncover(980, 1000, 60)).toBeNull()
    expect(scrollToUncover(1020, 1000, 0)).toBeNull()
  })

  it('does not scroll above the top of the content, nor when the view is there already', () => {
    expect(scrollToUncover(30, 20, 80)).toBe(0)
    expect(scrollToUncover(10, 0, 40)).toBeNull()
  })
})
