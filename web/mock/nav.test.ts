import { describe, expect, it } from 'vitest'
import { MockIndex, type NavPull } from './nav.ts'

const HEAD = 'b'.repeat(40)
const pull: NavPull = { head: HEAD, commits: { head: { sha: HEAD, tree: [] }, base: { sha: 'a'.repeat(40), tree: [] } } }

/** The status of the answer to a look-up of `name` in `path`: 400 for a request the server refuses, and 409 (not indexed) for one it takes. */
function lookUp(name: string, path = 'src/app.ts'): number | undefined {
  const query = new URLSearchParams({ head: HEAD, side: 'head', path, name })
  return new MockIndex(() => undefined).handle('GET', 'definitions', 1, query, null, pull)?.status
}

// The server's checks (`check_name`, `check_path` in `src/server/nav.rs`) are in terms of Rust's `char::is_control` and `char::is_whitespace`.
describe('the name of a look-up', () => {
  it.each([
    ['an ordinary name', 'slugify'],
    ['a name of another script', '\u{65e5}\u{672c}\u{8a9e}'],
    ['the name of a C++ operator, with its spaces', 'operator new[]'],
    ['the name of a C++ operator, with two words more', 'operator co_await'],
    ["a byte order mark, which is no whitespace to Rust (but is to JavaScript's `\\s`)", 'a\u{feff}b'],
    ['a zero width space, which is no whitespace to Rust either', 'a\u{200b}b'],
    ['a soft hyphen', 'a\u{ad}b'],
  ])('is taken: %s', (_what, name) => {
    expect(lookUp(name)).toBe(409)
  })

  it.each([
    ['nothing', ''],
    ['a space in a name', 'a b'],
    ['a space before the word operator', ' operator new'],
    ['two spaces in the name of an operator', 'operator  new'],
    ['a space at the end of the name of an operator', 'operator new '],
    ['a space in a word that only starts like operator', 'operators new'],
    ['a tab', 'a\tb'],
    ['a line feed', 'a\nb'],
    ['a null', 'a\x00b'],
    ['a delete', 'a\x7fb'],
    ['the first of the C1 control characters', 'a\x80b'],
    ['the next line, a C1 control character and whitespace', 'a\x85b'],
    ['the last of the C1 control characters', 'a\x9fb'],
    ['a no-break space', 'a\xa0b'],
    ['an ogham space mark', 'a\u{1680}b'],
    ['an em space', 'a\u{2003}b'],
    ['a line separator', 'a\u{2028}b'],
    ['a narrow no-break space', 'a\u{202f}b'],
    ['a medium mathematical space', 'a\u{205f}b'],
    ['an ideographic space', 'a\u{3000}b'],
    ['a name of 257 bytes', 'x'.repeat(257)],
  ])('is refused: %s', (_what, name) => {
    expect(lookUp(name)).toBe(400)
  })

  it('may be 256 bytes long', () => {
    expect(lookUp('x'.repeat(256))).toBe(409)
  })
})

describe('the path of a look-up', () => {
  it.each([
    ['an ordinary path', 'src/app.ts'],
    ['a path with a space', 'src/my app.ts'],
    ['a path with a no-break space: only control characters are refused', 'src/my\xa0app.ts'],
    ['a path with a byte order mark', 'src/\u{feff}app.ts'],
  ])('is taken: %s', (_what, path) => {
    expect(lookUp('slugify', path)).toBe(409)
  })

  it.each([
    ['nothing', ''],
    ['a tab', 'src/a\tb.ts'],
    ['a delete', 'src/a\x7fb.ts'],
    ['a C1 control character', 'src/a\x80b.ts'],
    ['the next line', 'src/a\x85b.ts'],
  ])('is refused: %s', (_what, path) => {
    expect(lookUp('slugify', path)).toBe(400)
  })
})
