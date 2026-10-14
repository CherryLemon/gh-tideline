import { describe, expect, it } from 'vitest'
import { identifierAt, type TokenKind } from './identifier'

function source(lines: string[], language = 'typescript', kindAt?: (line: number, column: number) => TokenKind | null) {
  return { getLanguageId: () => language, getLineContent: (line: number) => lines[line - 1], tokenKindAt: kindAt }
}

/** The identifier at a position of the first line, as `name@column-endColumn` (or null). */
function at(text: string, column: number, language = 'typescript'): string | null {
  const id = identifierAt(source([text], language), { lineNumber: 1, column })
  return id ? `${id.name}@${id.column}-${id.endColumn}` : null
}

describe('identifierAt', () => {
  it('finds the word under the position, with its columns', () => {
    expect(at('const total = parse(input)', 8)).toBe('total@7-12')
    expect(at('const total = parse(input)', 15)).toBe('parse@15-20')
    expect(identifierAt(source(['', 'let x = 1', '  value  ']), { lineNumber: 3, column: 5 })).toEqual({ name: 'value', line: 3, column: 3, endColumn: 8, nameColumn: 3 })
  })

  it('takes a position at either end of the word, as a caret does', () => {
    expect(at('parse(input)', 1)).toBe('parse@1-6')
    expect(at('parse(input)', 6)).toBe('parse@1-6')
    expect(at('parse(input)', 7)).toBe('input@7-12')
    expect(at('parse(input)', 12)).toBe('input@7-12')
    expect(at('parse(input)', 13)).toBeNull()
  })

  it('says where the name starts, also for a position at its end', () => {
    const start = (text: string, column: number, language = 'typescript') =>
      identifierAt(source([text], language), { lineNumber: 1, column })?.nameColumn ?? null
    // Monaco reports the right half of the last character as the column after it.
    for (const column of [7, 9, 11, 12]) expect(start('const total = 1', column)).toBe(7)
    expect(start('const total = 1', 13)).toBeNull()
  })

  it('finds nothing between words, in blanks or past the line', () => {
    expect(at('a  +  b', 4)).toBeNull()
    expect(at('', 1)).toBeNull()
    expect(at('   ', 2)).toBeNull()
    expect(at('foo', 5)).toBeNull()
    expect(at('foo', 0)).toBeNull()
  })

  it('splits names at punctuation', () => {
    expect(at('a.b::c->d', 3)).toBe('b@3-4')
    expect(at('a.b::c->d', 6)).toBe('c@6-7')
    expect(at('a.b::c->d', 9)).toBe('d@9-10')
    expect(at('snake_case + camelCase2', 4)).toBe('snake_case@1-11')
    expect(at('snake_case + camelCase2', 18)).toBe('camelCase2@14-24')
  })

  it('takes $ in a name for JavaScript, TypeScript and Java only', () => {
    expect(at('$el.find($scope)', 3, 'javascript')).toBe('$el@1-4')
    expect(at('const $ = jQuery', 7, 'typescript')).toBe('$@7-8')
    expect(at('Outer$Inner x', 3, 'java')).toBe('Outer$Inner@1-12')
    expect(at('Outer$Inner x', 3, 'rust')).toBe('Outer@1-6')
    expect(at('$var = 1', 3, 'python')).toBe('var@2-5')
  })

  it('takes letters of any script', () => {
    expect(at('let größe = 1', 6, 'rust')).toBe('größe@5-10')
    expect(at('名前 = 値', 2, 'python')).toBe('名前@1-3')
    expect(at('café.open()', 4, 'typescript')).toBe('café@1-5')
    expect(at('x = 𝒳y', 6, 'python')).toBe('𝒳y@5-8')
  })

  it('leaves out numbers, however they are written', () => {
    expect(at('x = 3.14', 5)).toBeNull()
    expect(at('x = 0xdeadBEEF', 7)).toBeNull()
    expect(at('x = 1_000', 6)).toBeNull()
    expect(at('x = 10n', 5)).toBeNull()
    expect(at('v2 = a1', 2)).toBe('v2@1-3')
  })

  it('leaves out words that are too long to be names', () => {
    const long = 'a'.repeat(1000)
    expect(at(`x = ${long}`, 200)).toBeNull()
    expect(at(`${long} = x`, 1)).toBeNull()
    expect(at(`${long} = x`, 1000 + 4)).toBe('x@1004-1005')
    const name = 'n'.repeat(256)
    expect(at(`${name}()`, 3)).toBe(`${name}@1-257`)
    expect(at(`${name}n()`, 3)).toBeNull()
    // The limit is in bytes.
    expect(at(`${'名'.repeat(86)} = 1`, 3, 'python')).toBeNull()
    expect(at(`${'名'.repeat(85)} = 1`, 3, 'python')).toBe(`${'名'.repeat(85)}@1-86`)
  })

  it('finds a name far along a line', () => {
    const line = `${'x + '.repeat(500)}target`
    expect(at(line, line.length - 2)).toBe(`target@${line.length - 5}-${line.length + 1}`)
  })

  describe('Rust raw identifiers', () => {
    it('gives the name without r# and covers all of it', () => {
      const text = 'let r#type = 1;'
      for (const column of [5, 6, 7, 8, 10, 11]) expect(at(text, column, 'rust')).toBe('type@5-11')
      expect(at(text, 4, 'rust')).toBe('let@1-4')
      expect(at(text, 12, 'rust')).toBeNull()
    })

    it('starts the name after the r#', () => {
      const start = (column: number) => identifierAt(source(['let r#type = 1;'], 'rust'), { lineNumber: 1, column })?.nameColumn
      for (const column of [5, 6, 7, 8, 10, 11]) expect(start(column)).toBe(7)
    })

    it('does not treat other r# as one', () => {
      expect(at('bar#type', 6, 'rust')).toBe('type@5-9')
      expect(at('x = r#"raw"#', 6, 'rust')).toBe('r@5-6')
      expect(at('r# = 1', 1, 'rust')).toBe('r@1-2')
      expect(at('let r#type = 1', 8, 'python')).toBe('type@7-11')
    })
  })

  describe('with token kinds', () => {
    const line = 'call(name) // call name'
    const kinds = (column: number): TokenKind => (column > 11 ? 'comment' : 'code')
    const find = (column: number, kindAt: (line: number, column: number) => TokenKind | null) =>
      identifierAt(source([line], 'typescript', kindAt), { lineNumber: 1, column })?.name ?? null

    it('skips names in comments, strings and regular expressions', () => {
      expect(find(2, (_l, c) => kinds(c))).toBe('call')
      expect(find(20, (_l, c) => kinds(c))).toBeNull()
      expect(find(2, () => 'string')).toBeNull()
      expect(find(2, () => 'regex')).toBeNull()
      expect(find(2, () => 'comment')).toBeNull()
    })

    it('asks about the first character of the name, on its line', () => {
      const asked: string[] = []
      const found = identifierAt(source(['x', '  foo.bar'], 'typescript', (l, c) => (asked.push(`${l}:${c}`), 'code')), { lineNumber: 2, column: 7 })
      expect(found?.name).toBe('bar')
      expect(asked).toEqual(['2:7'])
    })

    it('asks about the name of a raw identifier, not its r#', () => {
      const asked: number[] = []
      const found = identifierAt(source(['r#fn'], 'rust', (_l, c) => (asked.push(c), 'code')), { lineNumber: 1, column: 1 })
      expect(found).toEqual({ name: 'fn', line: 1, column: 1, endColumn: 5, nameColumn: 3 })
      expect(asked).toEqual([3])
    })

    it('goes on when the tokenizer cannot say', () => {
      expect(find(2, () => null)).toBe('call')
    })
  })
})
