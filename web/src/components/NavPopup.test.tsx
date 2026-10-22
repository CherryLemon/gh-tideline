// @vitest-environment jsdom
import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { NavHit } from '../api'
import { NavPopup } from './NavPopup'

const NAME = 'sendNotification'

let host: HTMLElement
let root: ReturnType<typeof createRoot>

beforeEach(() => {
  ;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  // jsdom lays nothing out and scrolls nothing into view.
  Element.prototype.scrollIntoView = vi.fn()
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})
afterEach(() => {
  act(() => root.unmount())
  host.remove()
  delete (Element.prototype as { scrollIntoView?: unknown }).scrollIntoView
})

/** A hit on the name that begins `at` characters into its line (columns count from 1). */
function hitAt(at: number): NavHit {
  const snippet = `${'x'.repeat(at)}${NAME}(user)`
  return { path: 'src/a.ts', line: 3, column: at + 1, endColumn: at + 1 + NAME.length, kind: 'usage', snippet, snippetColumn: at + 1, tier: 'file' }
}

const show = (...hits: NavHit[]) =>
  act(() => root.render(<NavPopup title="Usages" hits={hits} anchor={{ x: 10, y: 10 }} onPick={() => {}} onClose={() => {}} />))
const snippets = () => [...host.querySelectorAll('.nav-popup-snippet')].map((el) => el.textContent)
const marks = () => [...host.querySelectorAll('.nav-popup-snippet mark')].map((el) => el.textContent)

describe('NavPopup snippets', () => {
  it('show the line as it is up to a name 22 characters along it', () => {
    show(hitAt(0), hitAt(22))
    expect(snippets()).toEqual([hitAt(0).snippet, hitAt(22).snippet])
  })

  it('bring in a name further along, with 16 characters before it after an ellipsis', () => {
    show(hitAt(23), hitAt(60))
    expect(snippets()).toEqual([`…${'x'.repeat(16)}${NAME}(user)`, `…${'x'.repeat(16)}${NAME}(user)`])
  })

  it('mark the name wherever it is', () => {
    show(hitAt(0), hitAt(22), hitAt(23), hitAt(60))
    expect(marks()).toEqual([NAME, NAME, NAME, NAME])
  })

  it('show a line whose columns do not fit it as it is', () => {
    const hit = { ...hitAt(30), snippetColumn: 500 }
    show(hit)
    expect(snippets()).toEqual([hit.snippet])
    expect(marks()).toEqual([])
  })
})
