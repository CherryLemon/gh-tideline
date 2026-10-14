import { describe, expect, it } from 'vitest'
import type { LineChange } from '../threads'
import { mapLine } from './targets'

/** A change as Monaco reports it: original range, then modified range (end 0: nothing on that side). */
function change(os: number, oe: number, ms: number, me: number): LineChange {
  return { originalStartLineNumber: os, originalEndLineNumber: oe, modifiedStartLineNumber: ms, modifiedEndLineNumber: me }
}

const toModified = (changes: LineChange[], line: number) => mapLine(changes, line, 'original')
const toOriginal = (changes: LineChange[], line: number) => mapLine(changes, line, 'modified')

describe('mapLine', () => {
  it('keeps every line when nothing changed', () => {
    expect(toModified([], 1)).toBe(1)
    expect(toModified([], 42)).toBe(42)
    expect(toOriginal([], 7)).toBe(7)
  })

  describe('insertions', () => {
    // A B C -> A B X Y C: X Y come after original line 2.
    const changes = [change(2, 0, 3, 4)]

    it('shifts original lines after the insertion', () => {
      expect(toModified(changes, 1)).toBe(1)
      expect(toModified(changes, 2)).toBe(2)
      expect(toModified(changes, 3)).toBe(5)
      expect(toModified(changes, 9)).toBe(11)
    })

    it('maps inserted lines to the line they follow', () => {
      expect(toOriginal(changes, 3)).toBe(2)
      expect(toOriginal(changes, 4)).toBe(2)
    })

    it('shifts modified lines after the insertion back', () => {
      expect(toOriginal(changes, 2)).toBe(2)
      expect(toOriginal(changes, 5)).toBe(3)
    })
  })

  describe('deletions', () => {
    // A B X Y C -> A B C: X Y (original lines 3-4) are gone.
    const changes = [change(3, 4, 2, 0)]

    it('maps deleted lines to the line before them', () => {
      expect(toModified(changes, 3)).toBe(2)
      expect(toModified(changes, 4)).toBe(2)
    })

    it('shifts original lines after the deletion', () => {
      expect(toModified(changes, 2)).toBe(2)
      expect(toModified(changes, 5)).toBe(3)
      expect(toModified(changes, 8)).toBe(6)
    })

    it('shifts modified lines after the deletion back', () => {
      expect(toOriginal(changes, 2)).toBe(2)
      expect(toOriginal(changes, 3)).toBe(5)
    })
  })

  describe('modifications', () => {
    it('maps lines inside a change to its first line on the other side', () => {
      const changes = [change(3, 3, 3, 5)]
      expect(toModified(changes, 3)).toBe(3)
      expect(toOriginal(changes, 3)).toBe(3)
      expect(toOriginal(changes, 4)).toBe(3)
      expect(toOriginal(changes, 5)).toBe(3)
    })

    it('shifts the lines after a change by how much it grew or shrank', () => {
      const grown = [change(3, 3, 3, 5)]
      expect(toModified(grown, 4)).toBe(6)
      expect(toOriginal(grown, 6)).toBe(4)
      const shrunk = [change(5, 7, 5, 6)]
      expect(toModified(shrunk, 8)).toBe(7)
      expect(toOriginal(shrunk, 7)).toBe(8)
    })

    it('leaves the lines before a change alone', () => {
      const changes = [change(5, 7, 5, 6)]
      expect(toModified(changes, 4)).toBe(4)
      expect(toOriginal(changes, 1)).toBe(1)
    })

    it('shifts nothing when the change keeps its size', () => {
      const changes = [change(3, 4, 3, 4)]
      expect(toModified(changes, 5)).toBe(5)
      expect(toOriginal(changes, 100)).toBe(100)
    })
  })

  describe('several changes', () => {
    // Lines 1..10 become: 1 2 +a +b 3 4 5 (6 7 removed) 8 9->(c d e) 10
    const changes = [change(2, 0, 3, 4), change(6, 7, 7, 0), change(9, 9, 9, 11)]

    it('maps original lines', () => {
      const expected = [1, 2, 5, 6, 7, 7, 7, 8, 9, 12]
      expect(expected.map((_, i) => toModified(changes, i + 1))).toEqual(expected)
    })

    it('maps modified lines', () => {
      const expected = [1, 2, 2, 2, 3, 4, 5, 8, 9, 9, 9, 10]
      expect(expected.map((_, i) => toOriginal(changes, i + 1))).toEqual(expected)
    })

    it('round-trips the lines no change touches', () => {
      for (const line of [1, 2, 3, 4, 5, 8, 10]) expect(toOriginal(changes, toModified(changes, line))).toBe(line)
    })
  })

  describe('changes at the edges', () => {
    it('handles a removal at the top of the file', () => {
      // X Y A B -> A B
      const changes = [change(1, 2, 0, 0)]
      expect(toModified(changes, 1)).toBe(1)
      expect(toModified(changes, 2)).toBe(1)
      expect(toModified(changes, 3)).toBe(1)
      expect(toModified(changes, 4)).toBe(2)
      expect(toOriginal(changes, 1)).toBe(3)
      expect(toOriginal(changes, 2)).toBe(4)
    })

    it('handles an insertion at the top of the file', () => {
      // A B -> X Y A B
      const changes = [change(0, 0, 1, 2)]
      expect(toModified(changes, 1)).toBe(3)
      expect(toModified(changes, 2)).toBe(4)
      expect(toOriginal(changes, 1)).toBe(1)
      expect(toOriginal(changes, 2)).toBe(1)
      expect(toOriginal(changes, 3)).toBe(1)
      expect(toOriginal(changes, 4)).toBe(2)
    })

    it('handles an insertion at the end of the file', () => {
      // A B -> A B X
      const changes = [change(2, 0, 3, 3)]
      expect(toModified(changes, 2)).toBe(2)
      expect(toOriginal(changes, 3)).toBe(2)
    })

    it('handles a removal at the end of the file', () => {
      // A B X -> A B
      const changes = [change(3, 3, 2, 0)]
      expect(toModified(changes, 3)).toBe(2)
      expect(toOriginal(changes, 2)).toBe(2)
    })

    it('maps line 1 of a file changed from its first line', () => {
      const changes = [change(1, 2, 1, 3)]
      expect(toModified(changes, 1)).toBe(1)
      expect(toOriginal(changes, 1)).toBe(1)
      expect(toOriginal(changes, 3)).toBe(1)
      expect(toOriginal(changes, 4)).toBe(3)
    })
  })
})
