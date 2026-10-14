import { describe, expect, it } from 'vitest'
import type { ChangedFile } from '../api'
import type { LineChange } from '../threads'
import { mapLine, resolveTarget } from './targets'

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

function file(path: string, extra: Partial<ChangedFile> = {}): ChangedFile {
  return { path, status: 'M', oldOid: 'a', newOid: 'b', oldMode: '100644', newMode: '100644', binary: false, generated: false, ...extra }
}

describe('resolveTarget', () => {
  const modified = file('src/main.ts')
  const added = file('src/new.ts', { status: 'A', oldOid: null })
  const deleted = file('src/gone.ts', { status: 'D', newOid: null })
  const renamed = file('src/Button.tsx', { status: 'R', oldPath: 'src/Btn.tsx' })
  const copied = file('templates/email.html', { status: 'C', oldPath: 'templates/base.html' })
  const files = [modified, added, deleted, renamed, copied]
  const tree = new Set(['src/main.ts', 'src/new.ts', 'src/Button.tsx', 'templates/email.html', 'templates/base.html', 'src/util/log.ts'])

  it('finds the changed file for a place in the head', () => {
    expect(resolveTarget('src/main.ts', 'head', files, tree)).toEqual({ path: 'src/main.ts', side: 'head' })
    expect(resolveTarget('src/new.ts', 'head', files, tree)).toEqual({ path: 'src/new.ts', side: 'head' })
    expect(resolveTarget('src/Button.tsx', 'head', files, tree)).toEqual({ path: 'src/Button.tsx', side: 'head' })
    expect(resolveTarget('templates/email.html', 'head', files, tree)).toEqual({ path: 'templates/email.html', side: 'head' })
  })

  it('finds the changed file for a place in the base, on its base side', () => {
    expect(resolveTarget('src/main.ts', 'base', files, tree)).toEqual({ path: 'src/main.ts', side: 'base' })
    expect(resolveTarget('src/gone.ts', 'base', files, tree)).toEqual({ path: 'src/gone.ts', side: 'base' })
  })

  it('maps the old path of a renamed file to the file, on its base side', () => {
    expect(resolveTarget('src/Btn.tsx', 'base', files, tree)).toEqual({ path: 'src/Button.tsx', side: 'base' })
    expect(resolveTarget('src/Btn.tsx', 'head', files, tree)).toBeNull()
  })

  it('has a deleted file on the base side only', () => {
    expect(resolveTarget('src/gone.ts', 'head', files, tree)).toBeNull()
  })

  it('has an added file on the head side only', () => {
    expect(resolveTarget('src/new.ts', 'base', files, tree)).toBeNull()
  })

  it('does not take a renamed file for the one that has its new path in the base', () => {
    expect(resolveTarget('src/Button.tsx', 'base', files, tree)).toBeNull()
  })

  it('takes a file copied from for the file itself, not for its copy', () => {
    expect(resolveTarget('templates/base.html', 'base', files, tree)).toEqual({ path: 'templates/base.html', side: 'head' })
    expect(resolveTarget('templates/base.html', 'head', files, tree)).toEqual({ path: 'templates/base.html', side: 'head' })
    expect(resolveTarget('templates/email.html', 'base', files, tree)).toBeNull()
  })

  it('opens an unchanged file at the head, from either side', () => {
    expect(resolveTarget('src/util/log.ts', 'head', files, tree)).toEqual({ path: 'src/util/log.ts', side: 'head' })
    expect(resolveTarget('src/util/log.ts', 'base', files, tree)).toEqual({ path: 'src/util/log.ts', side: 'head' })
  })

  it('finds no file the pull request does not have', () => {
    expect(resolveTarget('src/other.ts', 'head', files, tree)).toBeNull()
    expect(resolveTarget('src/other.ts', 'base', files, tree)).toBeNull()
  })

  it('takes an unchanged file to be there while the head tree is not listed', () => {
    expect(resolveTarget('src/util/log.ts', 'head', files, null)).toEqual({ path: 'src/util/log.ts', side: 'head' })
    expect(resolveTarget('src/util/log.ts', 'base', files, null)).toEqual({ path: 'src/util/log.ts', side: 'head' })
    // Changed files need no listing.
    expect(resolveTarget('src/Btn.tsx', 'base', files, null)).toEqual({ path: 'src/Button.tsx', side: 'base' })
  })

  it('works for a pull request without changes', () => {
    expect(resolveTarget('a.ts', 'head', [], new Set(['a.ts']))).toEqual({ path: 'a.ts', side: 'head' })
    expect(resolveTarget('b.ts', 'head', [], new Set(['a.ts']))).toBeNull()
  })
})
