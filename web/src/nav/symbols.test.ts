import { describe, expect, it } from 'vitest'
import type { FileSymbols, RepoRef, SymbolInfo } from '../api'
import { createSymbolCache, definitionAt, enclosingScopes, nestSymbols, type SymbolNode } from './symbols'

function sym(name: string, startLine: number, endLine: number, extra: Partial<SymbolInfo> = {}): SymbolInfo {
  return { name, kind: 'function', line: startLine, column: 1, endColumn: 1 + name.length, startLine, endLine, ...extra }
}

/** A tree as nested names: `name` for a leaf, `[name, children]` otherwise. */
const shape = (nodes: SymbolNode[]): unknown[] => nodes.map((n) => (n.children.length > 0 ? [n.symbol.name, shape(n.children)] : n.symbol.name))

const FILE = [sym('A', 1, 20, { kind: 'class' }), sym('m1', 2, 8), sym('inner', 3, 5), sym('m2', 10, 15), sym('fn', 22, 30)]

describe('nestSymbols', () => {
  it('nests a definition in the one that spans its lines', () => {
    expect(shape(nestSymbols(FILE))).toEqual([['A', [['m1', ['inner']], 'm2']], 'fn'])
  })

  it('gives the same tree for any order of the input, and leaves the input alone', () => {
    const reversed = [...FILE].reverse()
    expect(shape(nestSymbols(reversed))).toEqual(shape(nestSymbols(FILE)))
    expect(reversed.map((s) => s.name)).toEqual(['fn', 'm2', 'inner', 'm1', 'A'])
  })

  it('keeps definitions on one line beside each other', () => {
    const list = [sym('a', 5, 5, { kind: 'constant' }), sym('b', 5, 5, { column: 12, endColumn: 13 }), sym('c', 6, 6)]
    expect(shape(nestSymbols(list))).toEqual(['a', 'b', 'c'])
    // Even a one-line function holds no other.
    expect(shape(nestSymbols([sym('outer', 4, 4), sym('nested', 4, 4, { column: 20, endColumn: 26 })]))).toEqual(['outer', 'nested'])
  })

  it('nests definitions with the same lines by where their names are', () => {
    const a = sym('a', 1, 5, { line: 1, column: 10, endColumn: 11 })
    const b = sym('b', 1, 5, { line: 1, column: 3, endColumn: 4 })
    expect(shape(nestSymbols([a, b]))).toEqual([['b', ['a']]])
    expect(shape(nestSymbols([b, a]))).toEqual([['b', ['a']]])
  })

  it('puts a definition that only partly overlaps another beside it', () => {
    expect(shape(nestSymbols([sym('A', 1, 10), sym('B', 5, 15)]))).toEqual(['A', 'B'])
    expect(shape(nestSymbols([sym('A', 1, 10), sym('B', 5, 15), sym('C', 6, 8)]))).toEqual(['A', ['B', ['C']]])
  })

  it('orders siblings by position', () => {
    expect(shape(nestSymbols([sym('z', 30, 40), sym('y', 10, 20), sym('x', 1, 5)]))).toEqual(['x', 'y', 'z'])
  })

  it('returns nothing for no symbols', () => {
    expect(nestSymbols([])).toEqual([])
  })
})

describe('enclosingScopes', () => {
  const names = (line: number) => enclosingScopes(FILE, line).map((s) => s.name)

  it('lists the scopes around a line, outermost first', () => {
    expect(names(4)).toEqual(['A', 'm1', 'inner'])
    expect(names(7)).toEqual(['A', 'm1'])
    expect(names(12)).toEqual(['A', 'm2'])
  })

  it('does not count the first line of a scope as inside it', () => {
    expect(names(1)).toEqual([])
    expect(names(2)).toEqual(['A'])
    expect(names(3)).toEqual(['A', 'm1'])
    expect(names(23)).toEqual(['fn'])
  })

  it('counts the last line, and nothing after it', () => {
    expect(names(8)).toEqual(['A', 'm1'])
    expect(names(9)).toEqual(['A'])
    expect(names(20)).toEqual(['A'])
    expect(names(21)).toEqual([])
    expect(names(31)).toEqual([])
  })

  it('does not depend on the order of the input', () => {
    expect(enclosingScopes([...FILE].reverse(), 4).map((s) => s.name)).toEqual(['A', 'm1', 'inner'])
  })

  it('never lists a definition on one line', () => {
    expect(enclosingScopes([sym('one', 5, 5)], 5)).toEqual([])
  })
})

describe('definitionAt', () => {
  const list = [sym('parse', 12, 40, { line: 12, column: 8, endColumn: 13 }), sym('x', 14, 14, { line: 14, column: 3, endColumn: 4 })]

  it('finds the definition whose name covers the position', () => {
    expect(definitionAt(list, 12, 8)?.name).toBe('parse')
    expect(definitionAt(list, 12, 10)?.name).toBe('parse')
    expect(definitionAt(list, 12, 12)?.name).toBe('parse')
    expect(definitionAt(list, 14, 3)?.name).toBe('x')
  })

  it('leaves out the column the name ends at, and everything off the name', () => {
    expect(definitionAt(list, 12, 13)).toBeUndefined()
    expect(definitionAt(list, 12, 7)).toBeUndefined()
    expect(definitionAt(list, 13, 8)).toBeUndefined()
  })

  it('uses the line of the name, not the first line of the definition', () => {
    const decorated = [sym('get', 9, 20, { line: 10, column: 3, endColumn: 6 })]
    expect(definitionAt(decorated, 9, 3)).toBeUndefined()
    expect(definitionAt(decorated, 10, 4)?.name).toBe('get')
  })
})

describe('createSymbolCache', () => {
  const repo: RepoRef = { host: 'github.com', owner: 'acme', repo: 'demo-app' }
  const oid = (n: number) => String(n).repeat(40).slice(0, 40)

  function counting(capacity: number) {
    const calls: string[] = []
    const get = createSymbolCache(async (_repo, id, path): Promise<FileSymbols> => {
      calls.push(`${id.slice(0, 1)}:${path}`)
      return { language: 'typescript', tooLarge: false, symbols: [sym(path, 1, 3)] }
    }, capacity)
    return { get, calls }
  }

  it('asks once for a file version, however many times and however soon it is asked for', async () => {
    const { get, calls } = counting(10)
    const [a, b] = await Promise.all([get(repo, oid(1), 'src/a.ts'), get(repo, oid(1), 'src/a.ts')])
    expect(a).toBe(b)
    expect(await get(repo, oid(1), 'src/a.ts')).toBe(a)
    expect(calls).toEqual(['1:src/a.ts'])
  })

  it('tells repositories, blobs and paths apart', async () => {
    const { get, calls } = counting(10)
    await get(repo, oid(1), 'src/a.ts')
    await get(repo, oid(2), 'src/a.ts')
    await get(repo, oid(1), 'src/b.ts')
    await get({ ...repo, repo: 'other' }, oid(1), 'src/a.ts')
    await get({ ...repo, host: 'ghe.example' }, oid(1), 'src/a.ts')
    expect(calls).toHaveLength(5)
  })

  it('forgets the least recently used file version beyond its capacity', async () => {
    const { get, calls } = counting(2)
    await get(repo, oid(1), 'a')
    await get(repo, oid(2), 'b')
    await get(repo, oid(1), 'a')
    await get(repo, oid(3), 'c')
    expect(calls).toEqual(['1:a', '2:b', '3:c'])
    await get(repo, oid(1), 'a')
    expect(calls).toHaveLength(3)
    await get(repo, oid(2), 'b')
    expect(calls).toEqual(['1:a', '2:b', '3:c', '2:b'])
  })

  it('does not keep a failure: the next call asks again', async () => {
    let attempts = 0
    const get = createSymbolCache(async (): Promise<FileSymbols> => {
      attempts++
      if (attempts === 1) throw new Error('boom')
      return { language: null, tooLarge: false, symbols: [] }
    }, 10)
    const first = get(repo, oid(1), 'a')
    const sharing = get(repo, oid(1), 'a')
    await expect(first).rejects.toThrow('boom')
    await expect(sharing).rejects.toThrow('boom')
    expect(attempts).toBe(1)
    expect(await get(repo, oid(1), 'a')).toEqual({ language: null, tooLarge: false, symbols: [] })
    expect(attempts).toBe(2)
    await get(repo, oid(1), 'a')
    expect(attempts).toBe(2)
  })
})
