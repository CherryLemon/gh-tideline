import { describe, expect, it } from 'vitest'
import type { ChangedFile, ReviewThread } from './api'
import { mkThread } from './test-fixtures'
import { fileCounts, hunkOf, origToMod, placeThreads, threadsForFile, type LineChange, type PlaceInput } from './threads'

function change(os: number, oe: number, ms: number, me: number): LineChange {
  return { originalStartLineNumber: os, originalEndLineNumber: oe, modifiedStartLineNumber: ms, modifiedEndLineNumber: me }
}

// Original 1..30 → modified: 5-6 replaced by 5-8, 12 inserted after 10 (mod 13), 20-22 deleted.
const CHANGES = [change(5, 6, 5, 8), change(10, 0, 13, 13), change(20, 22, 22, 0)]

function place(threads: ReviewThread[], over: Partial<PlaceInput> = {}) {
  return placeThreads({
    threads,
    view: 'split',
    lineChanges: CHANGES,
    lineCounts: { original: 30, modified: 30 },
    headOk: true,
    ...over,
  })
}

describe('origToMod', () => {
  it('shifts unchanged lines by the changes before them', () => {
    expect(origToMod(CHANGES, 1)).toEqual({ line: 1, deleted: false, lastAdded: null })
    expect(origToMod(CHANGES, 4)).toEqual({ line: 4, deleted: false, lastAdded: null })
    expect(origToMod(CHANGES, 7)).toEqual({ line: 9, deleted: false, lastAdded: null })
    // The line an insertion follows stays put; the next one moves past the insertion.
    expect(origToMod(CHANGES, 10)).toEqual({ line: 12, deleted: false, lastAdded: null })
    expect(origToMod(CHANGES, 11)).toEqual({ line: 14, deleted: false, lastAdded: null })
    expect(origToMod(CHANGES, 19)).toEqual({ line: 22, deleted: false, lastAdded: null })
    expect(origToMod(CHANGES, 23)).toEqual({ line: 23, deleted: false, lastAdded: null })
  })

  it('puts deleted lines where Monaco draws them: above the change on the modified side', () => {
    expect(origToMod(CHANGES, 5)).toEqual({ line: 4, deleted: true, lastAdded: 8 })
    expect(origToMod(CHANGES, 6)).toEqual({ line: 4, deleted: true, lastAdded: 8 })
    // A pure deletion follows modified line `modifiedStartLineNumber`.
    expect(origToMod(CHANGES, 21)).toEqual({ line: 22, deleted: true, lastAdded: null })
  })

  it('handles changes at the start of the file', () => {
    expect(origToMod([change(1, 2, 0, 0)], 1)).toEqual({ line: 0, deleted: true, lastAdded: null })
    expect(origToMod([change(1, 2, 0, 0)], 3)).toEqual({ line: 1, deleted: false, lastAdded: null })
    expect(origToMod([change(0, 0, 1, 3)], 1)).toEqual({ line: 4, deleted: false, lastAdded: null })
    expect(origToMod([], 7)).toEqual({ line: 7, deleted: false, lastAdded: null })
  })
})

describe('placeThreads', () => {
  it('puts each side in its own editor in split view', () => {
    const right = mkThread({ side: 'RIGHT', line: 12 })
    const left = mkThread({ side: 'LEFT', line: 21 })
    const p = place([right, left])
    expect(p.zones.map((z) => [z.thread.id, z.editor, z.afterLine])).toEqual([
      [right.id, 'modified', 12],
      [left.id, 'original', 21],
    ])
    // Folds open from the modified editor only: LEFT lines are mapped (21 is deleted).
    expect(p.zones.map((z) => z.reveal)).toEqual([12, 22])
    expect(p.panel).toEqual([])
  })

  it('highlights same-side ranges only', () => {
    const range = mkThread({ side: 'RIGHT', startSide: 'RIGHT', line: 8, startLine: 5 })
    const across = mkThread({ side: 'RIGHT', startSide: 'LEFT', line: 8, startLine: 5 })
    const p = place([range, across])
    expect(p.zones[0].highlight).toEqual({ from: 5, to: 8 })
    expect(p.zones[1].highlight).toBeNull()
    expect(p.zones[1].afterLine).toBe(8)
  })

  it('sends what it cannot place to the panel with the reason', () => {
    const file = mkThread({ subjectType: 'FILE', side: null, line: null })
    const outdated = mkThread({ line: null, originalLine: 40, isOutdated: true })
    const past = mkThread({ side: 'LEFT', line: 31 })
    const p = place([file, outdated, past])
    expect(p.zones).toEqual([])
    expect(p.panel.map((i) => [i.thread.id, i.reason])).toEqual([
      [file.id, 'file'],
      [outdated.id, 'outdated'],
      [past.id, 'out-of-range'],
    ])
    expect(place([mkThread()], { headOk: false }).panel[0].reason).toBe('head')
    expect(place([mkThread()], { view: null }).panel[0].reason).toBe('no-diff')
    // A file-level thread stays a file thread whatever the head.
    expect(place([file], { headOk: false }).panel[0].reason).toBe('file')
  })

  it('places LEFT threads under a renamed file’s old path, and nothing else from it', () => {
    const left = mkThread({ path: 'old.ts', side: 'LEFT', line: 3 })
    const right = mkThread({ path: 'old.ts', side: 'RIGHT', line: 3 })
    const p = place([left, right], { oldPath: 'old.ts' })
    expect(p.zones.map((z) => [z.thread.id, z.editor])).toEqual([[left.id, 'original']])
    expect(p.panel.map((i) => [i.thread.id, i.reason])).toEqual([[right.id, 'old-path']])
  })

  it('maps LEFT threads into the modified editor in unified view', () => {
    const context = mkThread({ side: 'LEFT', line: 7 })
    const replaced = mkThread({ side: 'LEFT', line: 6, startSide: 'LEFT', startLine: 5 })
    const removed = mkThread({ side: 'LEFT', line: 21 })
    const p = place([context, replaced, removed], { view: 'unified' })
    const z = new Map(p.zones.map((z) => [z.thread.id, z]))
    expect(z.get(context.id)).toMatchObject({ editor: 'modified', afterLine: 9, highlight: null, reveal: 9 })
    // After the lines that replaced it (5-8), so the old line numbers stay in line.
    expect(z.get(replaced.id)).toMatchObject({ editor: 'modified', afterLine: 8, highlight: null, reveal: 8 })
    // Nothing replaced 20-22: above Monaco's block of removed lines (default ordinal 10000) after line 22.
    expect(z.get(removed.id)).toMatchObject({ editor: 'modified', afterLine: 22, reveal: 22 })
    for (const zone of p.zones) expect(zone.ordinal).toBeLessThan(10000)
  })

  it('leaves line threads out until the diff is known', () => {
    const left = mkThread({ side: 'LEFT', line: 7 })
    const right = mkThread({ side: 'RIGHT', line: 7 })
    const file = mkThread({ subjectType: 'FILE', side: null, line: null })
    for (const view of ['split', 'unified'] as const) {
      const p = place([left, right, file], { view, lineChanges: null })
      expect(p.zones).toEqual([])
      expect(p.panel.map((i) => i.reason)).toEqual(['file'])
    }
  })

  it('does not reveal line 0 for deletions at the top', () => {
    const t = mkThread({ side: 'LEFT', line: 1 })
    const p = place([t], { view: 'unified', lineChanges: [change(1, 2, 0, 0)] })
    expect(p.zones[0].afterLine).toBe(0)
    expect(p.zones[0].reveal).toBeNull()
  })

  it('orders cards on the same line oldest first', () => {
    const newer = mkThread({ line: 12 })
    const older = mkThread({ line: 12 })
    newer.comments[0].createdAt = '2026-10-13T10:00:00Z'
    older.comments[0].createdAt = '2026-10-12T10:00:00Z'
    const p = place([newer, older])
    expect(p.zones.map((z) => z.thread.id)).toEqual([older.id, newer.id])
    expect(p.zones[0].ordinal).toBeLessThan(p.zones[1].ordinal)
  })
})

function mkFile(path: string, oldPath?: string): ChangedFile {
  return {
    path,
    oldPath,
    status: oldPath ? 'R' : 'M',
    oldOid: 'a',
    newOid: 'b',
    oldMode: '100644',
    newMode: '100644',
    binary: false,
    generated: false,
  }
}

describe('threadsForFile and fileCounts', () => {
  const renamed = mkFile('src/new.ts', 'src/old.ts')
  const plain = mkFile('src/a.ts')
  const threads = [
    mkThread({ path: 'src/new.ts' }),
    mkThread({ path: 'src/old.ts', side: 'LEFT', isResolved: true }),
    mkThread({ path: 'src/a.ts' }),
    mkThread({ path: 'src/gone.ts', line: null }),
  ]

  it('includes threads under the old path of a renamed file', () => {
    expect(threadsForFile(threads, renamed).map((t) => t.path)).toEqual(['src/new.ts', 'src/old.ts'])
    expect(threadsForFile(threads, plain).map((t) => t.path)).toEqual(['src/a.ts'])
  })

  it('counts threads and unresolved ones per file', () => {
    const c = fileCounts(threads, [renamed, plain])
    expect(c.get('src/new.ts')).toEqual({ threads: 2, unresolved: 1 })
    expect(c.get('src/a.ts')).toEqual({ threads: 1, unresolved: 1 })
    expect(c.get('src/gone.ts')).toEqual({ threads: 1, unresolved: 1 })
    expect(c.has('src/old.ts')).toBe(false)
  })

  it('does not alias an old path that is itself a file of the PR', () => {
    // a.ts was renamed to b.ts and a new a.ts was added.
    const c = fileCounts([mkThread({ path: 'src/a.ts' })], [mkFile('src/b.ts', 'src/a.ts'), mkFile('src/a.ts')])
    expect(c.get('src/a.ts')).toEqual({ threads: 1, unresolved: 1 })
    expect(c.has('src/b.ts')).toBe(false)
  })
})

describe('hunkOf', () => {
  it('keeps the last lines of the hunk without headers', () => {
    const t = mkThread({ diffHunk: '@@ -1,4 +1,5 @@ fn x\n a\n-b\n+c\n+d\n e\n\\ No newline at end of file' })
    expect(hunkOf(t)).toEqual([
      { kind: 'del', text: 'b' },
      { kind: 'add', text: 'c' },
      { kind: 'add', text: 'd' },
      { kind: 'ctx', text: 'e' },
    ])
    expect(hunkOf(t, 2).map((l) => l.text)).toEqual(['d', 'e'])
    expect(hunkOf(mkThread({ diffHunk: '' }))).toEqual([])
  })
})
