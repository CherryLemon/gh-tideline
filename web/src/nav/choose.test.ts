import { describe, expect, it } from 'vitest'
import type { NavHit, NavResult, SymbolInfo } from '../api'
import { choose, clickOn, type Click } from './choose'
import { identifierAt } from './identifier'

function hit(path: string, line: number, extra: Partial<NavHit> = {}): NavHit {
  return { path, line, column: 8, endColumn: 13, kind: 'function', snippet: 'fn parse()', snippetColumn: 4, tier: 'other', ...extra }
}

const result = (...hits: NavHit[]): NavResult => ({ language: 'rust', commit: 'c'.repeat(40), name: 'parse', hits, truncated: false })

const click: Click = { path: 'src/main.rs', side: 'head', line: 30, column: 17, name: 'parse' }

function symbol(name: string, line: number, column: number): SymbolInfo {
  return { name, kind: 'function', line, column, endColumn: column + name.length, startLine: line, endLine: line + 5 }
}

describe('choose', () => {
  it('jumps to the only definition', () => {
    const only = hit('src/parse.rs', 12)
    expect(choose(click, [], result(only))).toEqual({ kind: 'jump', hit: only })
  })

  it('jumps to the only definition in the file clicked in, whatever else the index has', () => {
    const here = hit('src/main.rs', 5, { tier: 'file' })
    expect(choose(click, [], result(hit('src/a.rs', 1), here, hit('src/b.rs', 2)))).toEqual({ kind: 'jump', hit: here })
  })

  it('offers a choice when the file clicked in has several, or none, of several', () => {
    const hits = [hit('src/main.rs', 5, { tier: 'file' }), hit('src/main.rs', 9, { tier: 'file' }), hit('src/a.rs', 1)]
    expect(choose(click, [], result(...hits))).toEqual({ kind: 'popup', hits })
    const elsewhere = [hit('src/a.rs', 1), hit('src/b.rs', 2)]
    expect(choose(click, [], result(...elsewhere))).toEqual({ kind: 'popup', hits: elsewhere })
  })

  it('keeps the order the server ranked them in', () => {
    const hits = [hit('src/z.rs', 1, { tier: 'dir' }), hit('src/a.rs', 1), hit('src/b.rs', 7)]
    const picked = choose(click, [], result(...hits))
    expect(picked.kind === 'popup' && picked.hits.map((h) => h.path)).toEqual(['src/z.rs', 'src/a.rs', 'src/b.rs'])
  })

  it('finds nothing when there is no definition', () => {
    expect(choose(click, [], result())).toEqual({ kind: 'none' })
  })

  it('asks for usages when the click is on the name of a definition of the file', () => {
    const symbols = [symbol('other', 3, 8), symbol('parse', 30, 17)]
    expect(choose(click, symbols, result(hit('src/a.rs', 1)))).toEqual({ kind: 'usages', name: 'parse' })
    expect(choose(click, symbols, result())).toEqual({ kind: 'usages', name: 'parse' })
    expect(choose({ ...click, column: 18 }, symbols, result())).toEqual({ kind: 'usages', name: 'parse' })
  })

  it('asks for the usages of the name the definition has, which the word clicked may be a part of', () => {
    const symbols = [symbol('operator==', 30, 17)]
    expect(choose({ ...click, name: 'operator' }, symbols, result())).toEqual({ kind: 'usages', name: 'operator==' })
  })

  it('does not take a name on another line or another column for the definition', () => {
    const symbols = [symbol('parse', 31, 17), symbol('parse', 30, 40)]
    expect(choose(click, symbols, result(hit('src/a.rs', 1)))).toEqual({ kind: 'jump', hit: hit('src/a.rs', 1) })
  })

  it('asks for usages when a definition the index found is where the click is', () => {
    const atClick = hit('src/main.rs', 30, { column: 17, endColumn: 22, tier: 'file' })
    expect(choose(click, [], result(atClick, hit('src/a.rs', 1)))).toEqual({ kind: 'usages', name: 'parse' })
    // Not another file's definition at the same place, nor one on the line next to it.
    expect(choose(click, [], result(hit('src/other.rs', 30, { column: 17, endColumn: 22 })))).toEqual({
      kind: 'jump',
      hit: hit('src/other.rs', 30, { column: 17, endColumn: 22 }),
    })
    expect(choose(click, [], result(hit('src/main.rs', 31, { column: 17, endColumn: 22 }))).kind).toBe('jump')
    expect(choose(click, [], result(hit('src/main.rs', 30, { column: 3, endColumn: 8 }))).kind).toBe('jump')
  })

  it('does not depend on the side', () => {
    const symbols = [symbol('parse', 30, 17)]
    expect(choose({ ...click, side: 'base' }, symbols, result())).toEqual({ kind: 'usages', name: 'parse' })
    expect(choose({ ...click, side: 'base' }, [], result(hit('src/parse.rs', 12))).kind).toBe('jump')
  })

  describe('for a click found in a line of the file', () => {
    // `parse` is the name at columns 17 to 21 of line 30; Monaco reports a click on the right half of its last
    // character as column 22, where no definition's name reaches.
    const text = 'export function parse(input: string) {'
    const clicked = (column: number, language = 'typescript'): Click => {
      const id = identifierAt({ getLanguageId: () => language, getLineContent: () => text }, { lineNumber: 30, column })
      return clickOn('src/main.ts', 'head', id!)
    }

    it('asks for usages wherever on the name of a definition the click is', () => {
      const symbols = [symbol('parse', 30, 17)]
      for (const column of [17, 19, 21, 22]) expect(choose(clicked(column), symbols, result(hit('src/a.rs', 1)))).toEqual({ kind: 'usages', name: 'parse' })
    })

    it('asks for usages when the index found the definition under the click, also at the end of the name', () => {
      const atClick = hit('src/main.ts', 30, { column: 17, endColumn: 22, tier: 'file' })
      expect(choose(clicked(22), [], result(atClick, hit('src/a.ts', 1)))).toEqual({ kind: 'usages', name: 'parse' })
    })

    it('starts the click at the name of a Rust raw identifier, as the index does', () => {
      const raw = 'fn r#type() {}'
      const id = identifierAt({ getLanguageId: () => 'rust', getLineContent: () => raw }, { lineNumber: 30, column: 5 })
      const click = clickOn('src/main.rs', 'head', id!)
      expect(click).toEqual({ path: 'src/main.rs', side: 'head', line: 30, column: 6, name: 'type' })
      expect(choose(click, [symbol('type', 30, 6)], result())).toEqual({ kind: 'usages', name: 'type' })
    })

    it('asks for the usages of a C++ operator by its whole name, from a click on the word operator', () => {
      const operator = 'bool operator==(const A&) const;'
      const id = identifierAt({ getLanguageId: () => 'cpp', getLineContent: () => operator }, { lineNumber: 30, column: 8 })
      const click = clickOn('src/a.h', 'head', id!)
      expect(click).toEqual({ path: 'src/a.h', side: 'head', line: 30, column: 6, name: 'operator' })
      // The index names the definition `operator==`, from where the word starts to the end of the `==`.
      expect(choose(click, [symbol('operator==', 30, 6)], result())).toEqual({ kind: 'usages', name: 'operator==' })
    })

    it('takes the place of the name from the end of it too, for a use of a name', () => {
      expect(clicked(22)).toEqual({ path: 'src/main.ts', side: 'head', line: 30, column: 17, name: 'parse' })
      expect(clicked(17)).toEqual(clicked(22))
    })
  })
})
