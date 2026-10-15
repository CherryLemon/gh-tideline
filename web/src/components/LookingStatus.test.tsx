// @vitest-environment jsdom
import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { GRACE_MS } from '../nav/build'
import { LookingStatus } from './LookingStatus'

let host: HTMLElement
let root: ReturnType<typeof createRoot>

beforeEach(() => {
  ;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  vi.useFakeTimers()
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})
afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.useRealTimers()
})

/** Draws it for a look-up that is under way, or is not. (Synchronous `act`s: they do not wait for a timer of the page.) */
const show = (looking: boolean) => act(() => root.render(<LookingStatus looking={looking} />))
const pass = (ms: number) =>
  act(() => {
    vi.advanceTimersByTime(ms)
  })
const region = () => host.querySelector('[role="status"]')
const text = () => region()?.textContent

describe('LookingStatus', () => {
  it('is a live region in the page, with no text, while there is no look-up', () => {
    show(false)
    expect(region()).not.toBeNull()
    expect(region()?.children.length).toBe(0)
    expect(text()).toBe('')
  })

  it('is there without its text from the start of a look-up: the text is what comes to it', () => {
    show(true)
    expect(region()).not.toBeNull()
    expect(text()).toBe('')
  })

  it('says that it is looking up once the look-up has lasted a moment', () => {
    show(true)
    pass(GRACE_MS - 1)
    expect(text()).toBe('')
    pass(1)
    expect(text()).toBe('Looking up…')
  })

  it('does not show a quick look-up, and not afterwards either', () => {
    const seen: (string | undefined)[] = []
    show(true)
    seen.push(text())
    pass(GRACE_MS - 1)
    seen.push(text())
    show(false)
    seen.push(text())
    pass(GRACE_MS * 3)
    seen.push(text())
    expect(seen).toEqual(['', '', '', ''])
  })

  it('takes the text away with the look-up', () => {
    show(true)
    pass(GRACE_MS)
    expect(text()).toBe('Looking up…')
    show(false)
    expect(text()).toBe('')
    pass(GRACE_MS * 3)
    expect(text()).toBe('')
  })

  it('is the same element throughout, which is how a screen reader comes to announce the change of its text', () => {
    show(false)
    const element = region()
    show(true)
    expect(region()).toBe(element)
    pass(GRACE_MS)
    expect(region()).toBe(element)
    show(false)
    expect(region()).toBe(element)
    expect(host.querySelectorAll('[role="status"]').length).toBe(1)
  })

  it('waits a moment of its own for the next look-up', () => {
    show(true)
    pass(GRACE_MS - 1)
    show(false)
    pass(1)
    show(true)
    pass(GRACE_MS - 1)
    expect(text()).toBe('')
    pass(1)
    expect(text()).toBe('Looking up…')
  })

  it('waits a moment of its own after a long look-up, too', () => {
    show(true)
    pass(GRACE_MS)
    expect(text()).toBe('Looking up…')
    show(false)
    show(true)
    expect(text()).toBe('')
    pass(GRACE_MS - 1)
    expect(text()).toBe('')
    pass(1)
    expect(text()).toBe('Looking up…')
  })

  it('stays as it is for a rendering that changes nothing', () => {
    show(true)
    pass(GRACE_MS - 100)
    show(true)
    pass(100)
    expect(text()).toBe('Looking up…')
  })

  it('leaves no timer behind', () => {
    show(true)
    expect(vi.getTimerCount()).toBe(1)
    show(false)
    expect(vi.getTimerCount()).toBe(0)
    show(true)
    act(() => root.unmount())
    expect(vi.getTimerCount()).toBe(0)
    root = createRoot(host)
  })
})
