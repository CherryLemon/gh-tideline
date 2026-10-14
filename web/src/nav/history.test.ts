import { describe, expect, it } from 'vitest'
import type { NavPos } from '../api'
import { newEntryId, pushedEntry, readEntry, replacedEntry } from './history'

const pos = (line: number, extra: Partial<NavPos> = {}): NavPos => ({ path: 'src/a.ts', side: 'head', line, column: 3, ...extra })

describe('readEntry', () => {
  it('reads an entry with and without a place', () => {
    expect(readEntry({ id: 'x1', depth: 0 })).toEqual({ id: 'x1', depth: 0 })
    expect(readEntry({ id: 'x1', depth: 4, pos: pos(10) })).toEqual({ id: 'x1', depth: 4, pos: pos(10) })
    expect(readEntry({ id: 'x1', depth: 1, pos: pos(2, { side: 'base' }) })?.pos?.side).toBe('base')
  })

  it('is null for anything the app did not store', () => {
    for (const state of [null, undefined, 'x1', 3, [], {}, { id: 'x1' }, { depth: 1 }]) expect(readEntry(state)).toBeNull()
  })

  it('needs a non-empty id and a depth that is a whole number from 0', () => {
    expect(readEntry({ id: '', depth: 0 })).toBeNull()
    expect(readEntry({ id: 7, depth: 0 })).toBeNull()
    expect(readEntry({ id: 'x1', depth: -1 })).toBeNull()
    expect(readEntry({ id: 'x1', depth: 1.5 })).toBeNull()
    expect(readEntry({ id: 'x1', depth: '1' })).toBeNull()
    expect(readEntry({ id: 'x1', depth: NaN })).toBeNull()
  })

  it('drops a malformed place and keeps the entry', () => {
    const bad: unknown[] = [
      'src/a.ts',
      { path: 'src/a.ts', side: 'head', line: 0, column: 1 },
      { path: 'src/a.ts', side: 'head', line: 1, column: 0 },
      { path: 'src/a.ts', side: 'head', line: 1.5, column: 1 },
      { path: 'src/a.ts', side: 'head', line: '1', column: 1 },
      { path: 'src/a.ts', side: 'left', line: 1, column: 1 },
      { path: '', side: 'head', line: 1, column: 1 },
      { path: 4, side: 'head', line: 1, column: 1 },
      { path: 'src/a.ts', side: 'head', line: 1 },
    ]
    for (const p of bad) expect(readEntry({ id: 'x1', depth: 2, pos: p })).toEqual({ id: 'x1', depth: 2 })
  })

  it('keeps only the known fields', () => {
    const read = readEntry({ id: 'x1', depth: 1, extra: true, pos: { ...pos(5), more: 1 } })
    expect(read).toEqual({ id: 'x1', depth: 1, pos: pos(5) })
  })
})

describe('pushedEntry', () => {
  it('is one level deeper, with its own id, and carries no place of the entry before it', () => {
    const before = { id: 'x1', depth: 2, pos: pos(10) }
    expect(pushedEntry(before, 'x2')).toEqual({ id: 'x2', depth: 3 })
    expect(before).toEqual({ id: 'x1', depth: 2, pos: pos(10) })
  })

  it('lands on the place it is given', () => {
    expect(pushedEntry({ id: 'x1', depth: 0 }, 'x2', pos(7))).toEqual({ id: 'x2', depth: 1, pos: pos(7) })
  })
})

describe('replacedEntry', () => {
  it('keeps the id and depth, and sets the place', () => {
    expect(replacedEntry({ id: 'x1', depth: 2 }, pos(4))).toEqual({ id: 'x1', depth: 2, pos: pos(4) })
    expect(replacedEntry({ id: 'x1', depth: 2, pos: pos(10) }, pos(4))).toEqual({ id: 'x1', depth: 2, pos: pos(4) })
  })

  it('keeps the place it had when given none', () => {
    expect(replacedEntry({ id: 'x1', depth: 2, pos: pos(10) })).toEqual({ id: 'x1', depth: 2, pos: pos(10) })
    expect(replacedEntry({ id: 'x1', depth: 2 })).toEqual({ id: 'x1', depth: 2 })
  })

  it('does not change what it is given', () => {
    const before = { id: 'x1', depth: 2, pos: pos(10) }
    replacedEntry(before, pos(4))
    expect(before.pos).toEqual(pos(10))
  })
})

describe('newEntryId', () => {
  it('is a readable entry id, different each time', () => {
    const ids = Array.from({ length: 50 }, newEntryId)
    expect(new Set(ids).size).toBe(50)
    for (const id of ids) {
      expect(id).toMatch(/^[0-9a-f]{16}$/)
      expect(readEntry({ id, depth: 0 })).not.toBeNull()
    }
  })
})
