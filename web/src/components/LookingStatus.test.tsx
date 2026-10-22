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

/** Draws it for the look-up `id`, which is under way, or is not. (Synchronous `act`s: they do not wait for a timer of the page.) */
const show = (looking: boolean, id = 1) => act(() => root.render(<LookingStatus looking={looking} id={id} />))
/**
 * Draws it for the look-up `id` that replaces the one before, as the page does: that one is ended and this one started in one go, so
 * that the first is drawn off, in a rendering that is never made, and the second is on, in the one that is.
 */
const replace = (id: number) =>
  act(() => {
    root.render(<LookingStatus looking={false} id={id - 1} />)
    root.render(<LookingStatus looking id={id} />)
  })
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

  it('waits a moment of its own for a look-up that replaces one that is shown, with no rendering between that has it off', () => {
    show(true, 1)
    pass(GRACE_MS)
    expect(text()).toBe('Looking up…')
    replace(2)
    expect(text()).toBe('')
    pass(GRACE_MS - 1)
    expect(text()).toBe('')
    pass(1)
    expect(text()).toBe('Looking up…')
  })

  it('waits the whole moment for a look-up that replaces one that has not lasted a moment yet', () => {
    show(true, 1)
    pass(GRACE_MS - 100)
    replace(2)
    pass(GRACE_MS - 1)
    expect(text()).toBe('')
    pass(1)
    expect(text()).toBe('Looking up…')
  })

  it('goes on replacing look-up after look-up, each with the moment of its own', () => {
    const seen: (string | undefined)[] = []
    show(true, 1)
    for (let id = 2; id <= 4; id++) {
      pass(GRACE_MS)
      seen.push(text())
      replace(id)
      seen.push(text())
    }
    pass(GRACE_MS)
    seen.push(text())
    expect(seen).toEqual(['Looking up…', '', 'Looking up…', '', 'Looking up…', '', 'Looking up…'])
  })

  it('takes the text away with a look-up that was a replacement, too', () => {
    show(true, 1)
    replace(2)
    pass(GRACE_MS)
    expect(text()).toBe('Looking up…')
    show(false, 2)
    expect(text()).toBe('')
    pass(GRACE_MS * 3)
    expect(text()).toBe('')
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

  it('leaves no timer of a look-up that was replaced', () => {
    show(true, 1)
    replace(2)
    expect(vi.getTimerCount()).toBe(1)
    pass(GRACE_MS)
    expect(vi.getTimerCount()).toBe(0)
  })
})
