// @vitest-environment jsdom
import { createElement, act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { NavPos } from './api'
import { currentEntry, navigate, rememberPos, useLocation } from './router'

const A = '/github.com/acme/demo/pull/1?file=a.ts'
const B = '/github.com/acme/demo/pull/1?file=b.ts'
const pos = (path: string, line: number): NavPos => ({ path, side: 'head', line, column: 1 })
const url = () => location.pathname + location.search
/** Resolves with the popstate that Back or Forward lead to. */
const popped = () => new Promise<void>((resolve) => window.addEventListener('popstate', () => resolve(), { once: true }))
/** What a browser does with a write to the history it finds too frequent: Chrome ignores it, Safari throws. */
const refusals = [
  ['ignores', () => {}],
  [
    'throws on',
    () => {
      throw new DOMException('Attempt to use history.replaceState() more than 100 times per 30 seconds', 'SecurityError')
    },
  ],
] as const

beforeEach(() => history.replaceState(null, '', A))
// The entry gets its state, if a test left it without one, so that the next test starts without an id of its own in waiting.
afterEach(() => {
  vi.restoreAllMocks()
  currentEntry()
})

describe('currentEntry', () => {
  it('gives the first entry an id and depth 0, once', () => {
    expect(history.state).toBeNull()
    const first = currentEntry()
    expect(first).toEqual({ id: expect.stringMatching(/^[0-9a-f]{16}$/), depth: 0 })
    expect(history.state).toEqual(first)
    expect(currentEntry()).toEqual(first)
    expect(url()).toBe(A)
  })

  it('starts over with a state the app did not write', () => {
    history.replaceState({ somebody: 'else' }, '')
    expect(currentEntry()).toEqual({ id: expect.any(String), depth: 0 })
    expect(history.state).toEqual(currentEntry())
  })

  it.each(refusals)('keeps the id it gave while the browser %s the write of the state', (_, refuse) => {
    vi.spyOn(history, 'replaceState').mockImplementation(refuse)
    const first = currentEntry()
    expect(currentEntry()).toEqual(first)
    expect(history.state).toBeNull()
    vi.restoreAllMocks()
    expect(currentEntry()).toEqual(first)
    expect(history.state).toEqual(first)
  })
})

describe('navigate', () => {
  it('pushes an entry one level deeper with an id of its own', () => {
    const before = currentEntry()
    const length = history.length
    navigate(B)
    expect(history.length).toBe(length + 1)
    expect(url()).toBe(B)
    expect(currentEntry()).toEqual({ id: expect.any(String), depth: 1 })
    expect(currentEntry().id).not.toBe(before.id)
  })

  it('pushes also to the URL it is at, and lands on the place it is given', () => {
    const before = currentEntry()
    const length = history.length
    navigate(A, false, pos('a.ts', 7))
    expect(history.length).toBe(length + 1)
    expect(currentEntry()).toEqual({ id: expect.any(String), depth: 1, pos: pos('a.ts', 7) })
    expect(currentEntry().id).not.toBe(before.id)
  })

  it('replaces in place, keeping the id, the depth and the place', () => {
    const first = currentEntry()
    rememberPos(pos('a.ts', 3))
    const length = history.length
    navigate(B, true)
    expect(history.length).toBe(length)
    expect(url()).toBe(B)
    expect(currentEntry()).toEqual({ ...first, pos: pos('a.ts', 3) })
  })

  it('replaces with the place it is given', () => {
    const first = currentEntry()
    navigate(B, true, pos('b.ts', 9))
    expect(currentEntry()).toEqual({ ...first, pos: pos('b.ts', 9) })
  })

  it('tells subscribers', () => {
    let told = 0
    const count = () => told++
    window.addEventListener('dr:navigate', count)
    navigate(B)
    navigate(A, true)
    window.removeEventListener('dr:navigate', count)
    expect(told).toBe(2)
  })

  it('leaves the URL as it is, and tells nobody, when the browser throws on the write', () => {
    const before = currentEntry()
    const [, refuse] = refusals[1]
    vi.spyOn(history, 'pushState').mockImplementation(refuse)
    vi.spyOn(history, 'replaceState').mockImplementation(refuse)
    let told = 0
    const count = () => told++
    window.addEventListener('dr:navigate', count)
    expect(() => navigate(B)).not.toThrow()
    expect(() => navigate(B, true)).not.toThrow()
    window.removeEventListener('dr:navigate', count)
    expect(told).toBe(0)
    vi.restoreAllMocks()
    expect(url()).toBe(A)
    expect(currentEntry()).toEqual(before)
  })
})

describe('rememberPos', () => {
  it('changes only the place of the entry: no new entry, no new URL, no notice', () => {
    const first = currentEntry()
    const length = history.length
    let told = 0
    const count = () => told++
    window.addEventListener('dr:navigate', count)
    rememberPos(pos('a.ts', 12))
    rememberPos(pos('a.ts', 13))
    window.removeEventListener('dr:navigate', count)
    expect(told).toBe(0)
    expect(history.length).toBe(length)
    expect(url()).toBe(A)
    expect(currentEntry()).toEqual({ ...first, pos: pos('a.ts', 13) })
  })

  it('initializes an entry that has no state', () => {
    rememberPos(pos('a.ts', 2))
    expect(currentEntry()).toEqual({ id: expect.any(String), depth: 0, pos: pos('a.ts', 2) })
  })

  it.each(refusals)('does not throw when the browser %s the write', (_, refuse) => {
    const first = currentEntry()
    vi.spyOn(history, 'replaceState').mockImplementation(refuse)
    expect(() => rememberPos(pos('a.ts', 4))).not.toThrow()
    vi.restoreAllMocks()
    expect(currentEntry()).toEqual(first)
  })
})

describe('Back and Forward', () => {
  it('find each entry as it was left', async () => {
    const first = currentEntry()
    rememberPos(pos('a.ts', 12))
    navigate(B)
    const second = currentEntry()
    rememberPos(pos('b.ts', 40))

    let arrived = popped()
    history.back()
    await arrived
    expect(url()).toBe(A)
    expect(currentEntry()).toEqual({ ...first, pos: pos('a.ts', 12) })

    arrived = popped()
    history.forward()
    await arrived
    expect(url()).toBe(B)
    expect(currentEntry()).toEqual({ ...second, pos: pos('b.ts', 40) })
  })

  it('read the depth of the entry they arrive at', async () => {
    navigate(B)
    navigate(A)
    expect(currentEntry().depth).toBe(2)
    const arrived = popped()
    history.back()
    await arrived
    expect(currentEntry().depth).toBe(1)
  })
})

describe('useLocation', () => {
  let root: ReturnType<typeof createRoot>
  const seen: string[] = []
  function Probe() {
    const { pathname, search, entryId } = useLocation()
    seen.push(`${pathname}${search}|${entryId}`)
    return null
  }

  beforeEach(async () => {
    ;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
    seen.length = 0
    root = createRoot(document.createElement('div'))
    await act(async () => root.render(createElement(Probe)))
  })
  afterEach(async () => {
    await act(async () => root.unmount())
  })

  it('reports the URL and the entry', () => {
    expect(seen.at(-1)).toBe(`${A}|${currentEntry().id}`)
  })

  it('reads the entry without writing the history', () => {
    expect(history.state).toBeNull()
    expect(seen.at(-1)).toBe(`${A}|${currentEntry().id}`)
    expect(history.state).toEqual(currentEntry())
  })

  it.each(refusals)('keeps one entry id while the browser %s the write of its state', async (_, refuse) => {
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {})
    vi.spyOn(history, 'replaceState').mockImplementation(refuse)
    const shown = seen.at(-1)
    await act(async () => root.render(null))
    seen.length = 0
    await act(async () => root.render(createElement(Probe)))
    await act(async () => {
      window.dispatchEvent(new Event('dr:navigate'))
      window.dispatchEvent(new Event('popstate'))
    })
    expect(new Set(seen)).toEqual(new Set([shown]))
    expect(`${A}|${currentEntry().id}`).toBe(shown)
    expect(history.state).toBeNull()
    expect(errors).not.toHaveBeenCalled()
  })

  it('re-renders for a push to the same URL, which has another entry id', async () => {
    const first = seen.at(-1)
    await act(async () => navigate(A))
    expect(seen.at(-1)).not.toBe(first)
    expect(seen.at(-1)?.split('|')[0]).toBe(A)
    expect(seen.at(-1)?.split('|')[1]).toBe(currentEntry().id)
  })

  it('does not re-render when only the place of the entry is recorded', async () => {
    const renders = seen.length
    await act(async () => rememberPos(pos('a.ts', 5)))
    expect(seen.length).toBe(renders)
  })

  it('follows Back', async () => {
    const first = seen.at(-1)
    await act(async () => navigate(B))
    expect(seen.at(-1)).not.toBe(first)
    const arrived = popped()
    await act(async () => {
      history.back()
      await arrived
    })
    expect(seen.at(-1)).toBe(first)
  })
})
