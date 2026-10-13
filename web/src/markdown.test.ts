import { describe, expect, it } from 'vitest'
import { markdownRenderer } from './markdown'

function fakeGitHub() {
  const asked: string[] = []
  const waiting = new Map<string, { resolve(html: string): void; reject(e: unknown): void }>()
  const fetchHtml = (text: string) => {
    asked.push(text)
    return new Promise<string>((resolve, reject) => waiting.set(text, { resolve, reject }))
  }
  const answer = (text: string, html = `<p>${text}</p>`) => waiting.get(text)!.resolve(html)
  const fail = (text: string) => waiting.get(text)!.reject(new Error('502'))
  return { asked, fetchHtml, answer, fail }
}

describe('markdownRenderer', () => {
  it('asks GitHub once per text, also while the first render is on its way', async () => {
    const gh = fakeGitHub()
    const md = markdownRenderer(gh.fetchHtml)
    const first = md.render('**a**')
    const second = md.render('**a**')
    expect(md.peek('**a**')).toBeUndefined()
    gh.answer('**a**')
    expect(await first).toBe('<p>**a**</p>')
    expect(await second).toBe('<p>**a**</p>')
    expect(md.peek('**a**')).toBe('<p>**a**</p>')
    expect(await md.render('**a**')).toBe('<p>**a**</p>')
    expect(gh.asked).toEqual(['**a**'])
  })

  it("doesn't keep a failed render: asking again retries", async () => {
    const gh = fakeGitHub()
    const md = markdownRenderer(gh.fetchHtml)
    const first = md.render('x')
    gh.fail('x')
    await expect(first).rejects.toThrow('502')
    expect(md.peek('x')).toBeUndefined()
    const again = md.render('x')
    gh.answer('x')
    expect(await again).toBe('<p>x</p>')
    expect(gh.asked).toEqual(['x', 'x'])
  })

  it('keeps the latest renders, the oldest used going first', async () => {
    const gh = fakeGitHub()
    const md = markdownRenderer(gh.fetchHtml, 2)
    for (const text of ['a', 'b']) {
      const p = md.render(text)
      gh.answer(text)
      await p
    }
    // Using `a` again makes `b` the oldest.
    await md.render('a')
    const c = md.render('c')
    gh.answer('c')
    await c
    expect(md.peek('a')).toBe('<p>a</p>')
    expect(md.peek('b')).toBeUndefined()
    expect(md.peek('c')).toBe('<p>c</p>')
  })
})
