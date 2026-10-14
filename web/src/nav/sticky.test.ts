import { describe, expect, it } from 'vitest'
import type { SymbolInfo } from '../api'
import { sameLines, scrollToUncover, stickyLines } from './sticky'

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

  it('does not pin a scope whose only line left in view is under the rows', () => {
    // At 50 three rows would cover the lines 50 to 52, the last of byWeight, and the line below them, 53, is in list only.
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

  it('has a header whose scopes are the scopes of the line below it, whenever it can', () => {
    for (let top = 1; top <= 80; top++) {
      const found = stickyLines(NESTED, below(top))
      const lineBelow = top + found.length
      const around = stickyLines(NESTED, () => lineBelow)
      // Either the rows are the scopes of the line below them, or no number of rows is (the scopes that end under the rows).
      const fixed = around.length === found.length && around.every((s, i) => s === found[i])
      const lower = found.length === 0 || stickyLines(NESTED, below(top), found.length - 1).length < found.length
      expect(fixed || lower, `top ${top}`).toBe(true)
    }
  })

  it('pins nothing where the view ends under the rows', () => {
    expect(stickyLines(NESTED, () => undefined)).toEqual([])
    expect(stickyLines(NESTED, (rows) => (rows === 0 ? 50 : undefined))).toEqual([])
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
