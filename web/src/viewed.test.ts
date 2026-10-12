import { describe, expect, it } from 'vitest'
import type { ViewedState } from './api'
import { ViewedSync, nextUnviewed, viewedMap } from './viewed'
import { fileQuery } from './router'

interface Call {
  path: string
  viewed: boolean
  resolve(state: ViewedState): void
  reject(e: unknown): void
}

function setup(initial: Record<string, ViewedState>) {
  const calls: Call[] = []
  const errors: string[] = []
  let changes = 0
  const sync = new ViewedSync(
    new Map(Object.entries(initial)),
    (path, viewed) =>
      new Promise<ViewedState>((resolve, reject) => {
        calls.push({ path, viewed, resolve, reject })
      }),
    () => changes++,
    (path) => errors.push(path),
  )
  const settle = () => new Promise((r) => setTimeout(r, 0))
  return { sync, calls, errors, changes: () => changes, settle }
}

describe('ViewedSync', () => {
  it('shows the intent at once and keeps the confirmed state', async () => {
    const { sync, calls, settle } = setup({ a: 'UNVIEWED' })
    sync.set('a', true)
    expect(sync.snapshot().get('a')).toBe('VIEWED')
    expect(calls.map((c) => [c.path, c.viewed])).toEqual([['a', true]])
    calls[0].resolve('VIEWED')
    await settle()
    expect(sync.snapshot().get('a')).toBe('VIEWED')
    expect(calls).toHaveLength(1)
  })

  it('rolls back and reports a failed request', async () => {
    const { sync, calls, errors, settle } = setup({ a: 'DISMISSED' })
    sync.set('a', true)
    calls[0].reject(new Error('boom'))
    await settle()
    expect(sync.snapshot().get('a')).toBe('DISMISSED')
    expect(errors).toEqual(['a'])
  })

  it('sends only the latest intent once the request in flight settles', async () => {
    const { sync, calls, settle } = setup({ a: 'UNVIEWED' })
    sync.set('a', true)
    sync.set('a', false)
    sync.set('a', true)
    sync.set('a', false)
    expect(calls).toHaveLength(1)
    calls[0].resolve('VIEWED')
    await settle()
    expect(calls.map((c) => c.viewed)).toEqual([true, false])
    expect(sync.snapshot().get('a')).toBe('UNVIEWED')
    calls[1].resolve('UNVIEWED')
    await settle()
    expect(calls).toHaveLength(2)
    expect(sync.snapshot().get('a')).toBe('UNVIEWED')
  })

  it('skips the request when toggled back before it was needed', async () => {
    const { sync, calls, settle } = setup({ a: 'UNVIEWED' })
    sync.set('a', true)
    sync.set('a', false)
    calls[0].resolve('VIEWED')
    await settle()
    // Confirmed VIEWED but the user wants it unviewed: one more request.
    expect(calls.map((c) => c.viewed)).toEqual([true, false])
  })

  it('ignores paths GitHub does not list and stops notifying after dispose', async () => {
    const { sync, calls, changes, settle } = setup({ a: 'UNVIEWED' })
    sync.set('not-listed', true)
    expect(calls).toHaveLength(0)
    expect(sync.has('not-listed')).toBe(false)
    sync.set('a', true)
    const before = changes()
    sync.dispose()
    calls[0].reject(new Error('late'))
    await settle()
    expect(changes()).toBe(before)
  })

  it('sends nothing more once disposed, even if an intent is pending', async () => {
    const { sync, calls, settle } = setup({ a: 'UNVIEWED' })
    sync.set('a', true)
    sync.set('a', false)
    sync.dispose()
    calls[0].resolve('VIEWED')
    await settle()
    expect(calls).toHaveLength(1)
  })
})

describe('ViewedSync.rebase', () => {
  it('takes server marks for idle paths and keeps busy ones', async () => {
    const { sync, calls } = setup({ a: 'UNVIEWED', b: 'UNVIEWED', c: 'VIEWED' })
    sync.set('a', true)
    sync.rebase(
      new Map<string, ViewedState>([
        ['a', 'UNVIEWED'],
        ['b', 'VIEWED'],
      ]),
      performance.now(),
    )
    const marks = sync.snapshot()
    expect(marks.get('a')).toBe('VIEWED') // in flight: intent wins
    expect(marks.get('b')).toBe('VIEWED') // changed elsewhere: taken
    expect(marks.has('c')).toBe(false) // no longer listed
    expect(sync.has('c')).toBe(false)
    expect(calls).toHaveLength(1)
  })

  it('ignores a list requested before the last request for a path settled', async () => {
    const { sync, calls, settle } = setup({ a: 'UNVIEWED' })
    const before = performance.now() - 1
    sync.set('a', true)
    calls[0].resolve('VIEWED')
    await settle()
    // A refresh sent before the mark landed still reports the old state.
    sync.rebase(new Map<string, ViewedState>([['a', 'UNVIEWED']]), before)
    expect(sync.snapshot().get('a')).toBe('VIEWED')
    // A later one is authoritative (e.g. unmarked on github.com).
    sync.rebase(new Map<string, ViewedState>([['a', 'UNVIEWED']]), performance.now() + 1)
    expect(sync.snapshot().get('a')).toBe('UNVIEWED')
  })

  it('ignores a list requested before one already taken', () => {
    const { sync } = setup({ a: 'UNVIEWED' })
    const t = performance.now()
    sync.rebase(new Map<string, ViewedState>([['a', 'VIEWED']]), t + 2)
    sync.rebase(new Map<string, ViewedState>([['a', 'UNVIEWED']]), t + 1)
    expect(sync.snapshot().get('a')).toBe('VIEWED')
  })

  it('drops the intent of a path no longer listed, also once its request settles', async () => {
    const { sync, calls, settle } = setup({ a: 'UNVIEWED', b: 'UNVIEWED' })
    sync.set('a', true)
    sync.set('a', false)
    sync.rebase(new Map<string, ViewedState>([['b', 'UNVIEWED']]), performance.now())
    expect(sync.snapshot().has('a')).toBe(false)
    calls[0].resolve('VIEWED')
    await settle()
    expect(sync.snapshot().has('a')).toBe(false)
    expect(sync.has('a')).toBe(false)
    expect(calls).toHaveLength(1)
  })
})

describe('nextUnviewed', () => {
  const order = ['a', 'b', 'c', 'd'].map((path) => ({ path }))

  it('finds the next file that is not viewed, wrapping around', () => {
    const marks = new Map<string, ViewedState>([
      ['a', 'UNVIEWED'],
      ['b', 'VIEWED'],
      ['c', 'VIEWED'],
      ['d', 'DISMISSED'],
    ])
    expect(nextUnviewed(order, 0, marks)).toBe(3)
    expect(nextUnviewed(order, 3, marks)).toBe(0)
  })

  it('skips files without a mark and returns null when everything is viewed', () => {
    const marks = new Map<string, ViewedState>([
      ['a', 'VIEWED'],
      ['c', 'VIEWED'],
    ])
    expect(nextUnviewed(order, 0, marks)).toBeNull()
    expect(nextUnviewed([], 0, marks)).toBeNull()
  })

  it('prefers files that are not generated', () => {
    const files = [
      { path: 'a', generated: false },
      { path: 'lock', generated: true },
      { path: 'b', generated: false },
    ]
    const marks = new Map<string, ViewedState>([
      ['a', 'VIEWED'],
      ['lock', 'UNVIEWED'],
      ['b', 'UNVIEWED'],
    ])
    expect(nextUnviewed(files, 0, marks)).toBe(2)
    marks.set('b', 'VIEWED')
    expect(nextUnviewed(files, 2, marks)).toBe(1)
  })
})

describe('viewedMap', () => {
  it('keeps only files that carry a mark', () => {
    const base = { status: 'M', oldOid: null, newOid: null, oldMode: '100644', newMode: '100644', binary: false, generated: false } as const
    const m = viewedMap([
      { ...base, path: 'a', viewed: 'VIEWED' },
      { ...base, path: 'b' },
    ])
    expect([...m]).toEqual([['a', 'VIEWED']])
  })
})

describe('fileQuery', () => {
  it('keeps the range parameters and replaces file', () => {
    expect(fileQuery('src/a b.ts', '?file=old&from=1&to=2')).toBe('?file=src/a%20b.ts&from=1&to=2')
    expect(fileQuery('x', '?range=since-review')).toBe('?file=x&range=since-review')
    expect(fileQuery('x')).toBe('?file=x')
  })
})
