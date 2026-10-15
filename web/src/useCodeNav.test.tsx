// @vitest-environment jsdom
import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LookingStatus } from './components/LookingStatus'
import type { NavSource } from './monaco/links'
import { GRACE_MS } from './nav/build'
import { useCodeNav, type CodeNav, type CodeNavDeps } from './useCodeNav'

let host: HTMLElement
let root: ReturnType<typeof createRoot>
/** What the page last had from the hook. */
let nav: CodeNav

const deps: CodeNavDeps = {
  repo: { host: 'github.com', owner: 'octo', repo: 'hello' },
  prNumber: 7,
  head: 'abc',
  files: [],
  tree: null,
  goTo: () => {},
  showToast: () => {},
  onRefresh: () => {},
  entryId: 'entry',
}

const source: NavSource = { click: { path: 'a.ts', side: 'head', line: 1, column: 1, name: 'foo' }, oid: null, anchor: { x: 0, y: 0 } }

/** The status bar of the page, which says that a look-up is under way. */
function Page() {
  nav = useCodeNav(deps)
  return <LookingStatus looking={nav.looking} id={nav.lookId} />
}

beforeEach(() => {
  ;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  vi.useFakeTimers()
  // A server that never answers: a request ends only with the reason of its signal, as with `fetch`.
  vi.stubGlobal(
    'fetch',
    (_path: string, init: RequestInit) => new Promise((_, reject) => init.signal?.addEventListener('abort', () => reject(init.signal?.reason))),
  )
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  act(() => root.render(<Page />))
})
afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

const pass = (ms: number) =>
  act(() => {
    vi.advanceTimersByTime(ms)
  })
const text = () => host.querySelector('[role="status"]')?.textContent

describe('useCodeNav', () => {
  it('has a look-up of its own for each request: the id is new, and the look-up is on throughout a replacement', () => {
    expect(nav).toMatchObject({ looking: false, lookId: 0 })
    act(() => nav.request(source))
    expect(nav).toMatchObject({ looking: true, lookId: 1 })
    act(() => nav.request(source))
    expect(nav).toMatchObject({ looking: true, lookId: 2 })
    act(() => nav.dismiss())
    expect(nav).toMatchObject({ looking: false, lookId: 2 })
  })

  it('says that it is looking up once a look-up has lasted a moment', () => {
    act(() => nav.request(source))
    pass(GRACE_MS - 1)
    expect(text()).toBe('')
    pass(1)
    expect(text()).toBe('Looking up…')
  })

  it('gives a look-up that replaces one under way a moment of its own before it says so', () => {
    act(() => nav.request(source))
    pass(GRACE_MS)
    expect(text()).toBe('Looking up…')
    act(() => nav.request(source))
    expect(text()).toBe('')
    pass(GRACE_MS - 1)
    expect(text()).toBe('')
    pass(1)
    expect(text()).toBe('Looking up…')
  })

  it('says nothing once the look-up is ended', () => {
    act(() => nav.request(source))
    pass(GRACE_MS)
    act(() => nav.dismiss())
    expect(text()).toBe('')
    pass(GRACE_MS * 3)
    expect(text()).toBe('')
  })
})
