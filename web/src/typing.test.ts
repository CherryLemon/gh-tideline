// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { isTypingTarget } from './viewed'

function el(html: string, selector: string): Element {
  const box = document.createElement('div')
  box.innerHTML = html
  return box.querySelector(selector)!
}

describe('isTypingTarget', () => {
  it('counts text fields, not other inputs', () => {
    expect(isTypingTarget(el('<input type="text">', 'input'))).toBe(true)
    expect(isTypingTarget(el('<input>', 'input'))).toBe(true)
    expect(isTypingTarget(el('<textarea></textarea>', 'textarea'))).toBe(true)
    expect(isTypingTarget(el('<input type="checkbox">', 'input'))).toBe(false)
    expect(isTypingTarget(el('<button>x</button>', 'button'))).toBe(false)
    expect(isTypingTarget(null)).toBe(false)
  })

  it('ignores the editor’s own input but not its find box or review cards', () => {
    const editor = (inner: string) => `<div class="monaco-editor">${inner}</div>`
    expect(isTypingTarget(el(editor('<textarea class="inputarea"></textarea>'), 'textarea'))).toBe(false)
    expect(isTypingTarget(el(editor('<div class="monaco-inputbox"><textarea></textarea></div>'), 'textarea'))).toBe(true)
    expect(isTypingTarget(el(editor('<div class="tl-zone"><div><textarea></textarea></div></div>'), 'textarea'))).toBe(true)
    expect(isTypingTarget(el(editor('<div class="tl-zone"><button>Reply</button></div>'), 'button'))).toBe(false)
  })
})
