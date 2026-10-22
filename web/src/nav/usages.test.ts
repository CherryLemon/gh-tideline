import { describe, expect, it } from 'vitest'
import type { NavHit } from '../api'
import { declarationsText, usagesText } from './usages'

const hit = (line: number): NavHit => ({ path: 'src/a.ts', line, column: 1, endColumn: 4, kind: 'call', snippet: 'foo()', snippetColumn: 1, tier: 'file' })
const hits = (n: number) => Array.from({ length: n }, (_, i) => hit(i + 1))

describe('declarationsText', () => {
  it('titles the list with the name and the count, and has no note for a list that is complete', () => {
    expect(declarationsText('parse', { hits: hits(3), truncated: false })).toEqual({ title: 'Choose declaration of parse (3)' })
    expect(declarationsText('parse', { hits: hits(2), truncated: false }).note).toBeUndefined()
  })

  it('counts a list the server cut short as "N+" and says so under the rows', () => {
    expect(declarationsText('new', { hits: hits(200), truncated: true })).toEqual({
      title: 'Choose declaration of new (200+)',
      note: 'Only the first 200 declarations are listed',
    })
  })

  it('puts the name in as it is', () => {
    expect(declarationsText('$el_2', { hits: hits(2), truncated: false }).title).toBe('Choose declaration of $el_2 (2)')
  })
})

describe('usagesText', () => {
  it('titles the list with the name and the count, and says the usages are matched by name', () => {
    expect(usagesText('slugify', { hits: hits(6), truncated: false })).toEqual({ title: 'Usages of slugify · matched by name (6)', empty: 'No usages found' })
    expect(usagesText('x', { hits: hits(1), truncated: false }).title).toBe('Usages of x · matched by name (1)')
  })

  it('says nothing more for a list that is complete', () => {
    expect(usagesText('slugify', { hits: hits(6), truncated: false }).note).toBeUndefined()
  })

  it('counts a list the server cut short as "N+" and says so under the rows', () => {
    const text = usagesText('ping', { hits: hits(1000), truncated: true })
    expect(text.title).toBe('Usages of ping · matched by name (1000+)')
    expect(text.note).toBe('Only the first 1000 usages are listed')
  })

  it('has a text and a hint for a name nothing uses', () => {
    expect(usagesText('summarize', { hits: [], truncated: false })).toEqual({
      title: 'Usages of summarize · matched by name (0)',
      empty: 'No usages found',
      note: "Usages are matched by name in this language's files",
    })
  })

  it('puts the name in as it is', () => {
    expect(usagesText('$el_2', { hits: [], truncated: false }).title).toBe('Usages of $el_2 · matched by name (0)')
    expect(usagesText('operator==', { hits: hits(2), truncated: false }).title).toBe('Usages of operator== · matched by name (2)')
  })
})
