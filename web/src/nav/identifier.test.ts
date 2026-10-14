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
      const text = 'foo r#type = 1;'
      for (const column of [5, 6, 7, 8, 10, 11]) expect(at(text, column, 'rust')).toBe('type@5-11')
      expect(at(text, 4, 'rust')).toBe('foo@1-4')
      expect(at(text, 12, 'rust')).toBeNull()
    })

    it('is a name also when what follows the r# is a keyword', () => {
      expect(at('let r#fn = r#match;', 6, 'rust')).toBe('fn@5-9')
      expect(at('let r#fn = r#match;', 14, 'rust')).toBe('match@12-19')
    })

    it('starts the name after the r#', () => {
      const start = (column: number) => identifierAt(source(['let r#type = 1;'], 'rust'), { lineNumber: 1, column })?.nameColumn
      for (const column of [5, 6, 7, 8, 10, 11]) expect(start(column)).toBe(7)
    })

    it('does not treat other r# as one', () => {
      expect(at('bar#name', 6, 'rust')).toBe('name@5-9')
      expect(at('x = r#"raw"#', 6, 'rust')).toBe('r@5-6')
      expect(at('r# = 1', 1, 'rust')).toBe('r@1-2')
      expect(at('let r#type = 1', 8, 'python')).toBe('type@7-11')
    })
  })

  describe('keywords', () => {
    it('are no names', () => {
      expect(at('export const total = 1', 3)).toBeNull()
      expect(at('export const total = 1', 10)).toBeNull()
      expect(at('export const total = 1', 15)).toBe('total@14-19')
      expect(at('  if (ok) return value', 4)).toBeNull()
      expect(at('  if (ok) return value', 14)).toBeNull()
      expect(at('  if (ok) return value', 20)).toBe('value@18-23')
      expect(at('x = flag ? null : new Map()', 13)).toBeNull()
      expect(at('x = flag ? null : new Map()', 21)).toBeNull()
      expect(at('x = flag ? null : new Map()', 25)).toBe('Map@23-26')
    })

    it('are also those at either end of the word, as the caret has it', () => {
      expect(at('if (ok)', 1)).toBeNull()
      expect(at('if (ok)', 3)).toBeNull()
    })

    it('are names when they only look like one', () => {
      expect(at('export_all(returned, iffy)', 3)).toBe('export_all@1-11')
      expect(at('export_all(returned, iffy)', 14)).toBe('returned@12-20')
      expect(at('export_all(returned, iffy)', 23)).toBe('iffy@22-26')
      expect(at('Return(Export)', 2)).toBe('Return@1-7')
    })

    it('are those of the language of the file', () => {
      expect(at('self.total = None', 2, 'python')).toBe('self@1-5')
      expect(at('self.total = None', 14, 'python')).toBeNull()
      expect(at('self.total = None', 2, 'rust')).toBeNull()
      expect(at('x = none', 5, 'python')).toBe('none@5-9')
      expect(at('func main() {', 2, 'go')).toBeNull()
      expect(at('func main() {', 8, 'go')).toBe('main@6-10')
      expect(at('func main() {', 2, 'rust')).toBe('func@1-5')
      expect(at('if err != nil {', 12, 'go')).toBe('nil@11-14')
      expect(at('public static void main(String[] args) {', 3, 'java')).toBeNull()
      expect(at('public static void main(String[] args) {', 15, 'java')).toBeNull()
      expect(at('public static void main(String[] args) {', 23, 'java')).toBe('main@20-24')
      expect(at('final var record = null;', 3, 'java')).toBeNull()
      expect(at('final var record = null;', 9, 'java')).toBe('var@7-10')
      expect(at('final var record = null;', 13, 'java')).toBe('record@11-17')
      expect(at('static int total;', 3, 'c')).toBeNull()
      expect(at('static int total;', 9, 'c')).toBeNull()
      expect(at('struct node *new = list->class;', 16, 'c')).toBe('new@14-17')
      expect(at('struct node *new = list->class;', 29, 'c')).toBe('class@26-31')
      expect(at('class Foo : public Bar {', 3, 'cpp')).toBeNull()
      expect(at('class Foo : public Bar {', 15, 'cpp')).toBeNull()
      expect(at('auto p = new Foo(nullptr);', 12, 'cpp')).toBeNull()
      expect(at('auto p = new Foo(nullptr);', 22, 'cpp')).toBeNull()
    })

    it('are not those of a language that is not known', () => {
      expect(at('export default total', 3, 'plaintext')).toBe('export@1-7')
      expect(at('return value', 3, 'json')).toBe('return@1-7')
    })

    it('leave out the words that only some places make one', () => {
      expect(at('type Total = string', 2)).toBe('type@1-5')
      expect(at("import { get } from './store'", 10)).toBe('get@10-13')
      expect(at("import { get } from './store'", 17)).toBe('from@16-20')
      expect(at('async function load() {}', 3)).toBe('async@1-6')
      expect(at('match value:', 3, 'python')).toBe('match@1-6')
      expect(at('union Value { a: u8 }', 3, 'rust')).toBe('union@1-6')
      expect(at('void run() override', 17, 'cpp')).toBe('override@12-20')
      expect(at('struct Foo final : Bar {}', 13, 'cpp')).toBe('final@12-17')
    })

    it('leave out the operator of C++, which operator== is looked up by', () => {
      expect(at('bool operator==(const Foo&) const;', 8, 'cpp')).toBe('operator@6-14')
    })

    it('are names after a dot in JavaScript and TypeScript, where a property may be called one', () => {
      expect(at('cache.delete(key)', 8)).toBe('delete@7-13')
      expect(at('promise.catch(fail).finally(done)', 10, 'javascript')).toBe('catch@9-14')
      expect(at('promise.catch(fail).finally(done)', 23, 'javascript')).toBe('finally@21-28')
      expect(at('options?.default', 12)).toBe('default@10-17')
      expect(at('  .delete(key)', 6)).toBe('delete@4-10')
    })

    it('are no names after the three dots of a spread, or a dot in another language', () => {
      expect(at('[...this.items]', 6)).toBeNull()
      expect(at('f(...new Set())', 7)).toBeNull()
      expect(at('self.None', 7, 'python')).toBeNull()
      expect(at('Foo.class', 6, 'java')).toBeNull()
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
