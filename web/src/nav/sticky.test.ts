import { describe, expect, it } from 'vitest'
import type { SymbolInfo } from '../api'
import { stickyLines } from './sticky'

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

const rows = (list: SymbolInfo[], top: number, max?: number) => stickyLines(list, top, max).map((s) => s.name)

describe('stickyLines', () => {
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
