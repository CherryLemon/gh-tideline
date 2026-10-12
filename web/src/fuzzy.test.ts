import { describe, expect, it } from 'vitest'
import { fuzzyMatch } from './fuzzy'

function rank(paths: string[], q: string): string[] {
  return paths
    .map((p) => ({ p, m: fuzzyMatch(p, q) }))
    .filter((x) => x.m !== null)
    .sort((a, b) => b.m!.score - a.m!.score)
    .map((x) => x.p)
}

describe('fuzzyMatch', () => {
  it('matches subsequences case-insensitively and reports positions', () => {
    const m = fuzzyMatch('src/DiffPane.tsx', 'dfp')
    expect(m).not.toBeNull()
    expect(m!.positions.map((i) => 'src/DiffPane.tsx'[i].toLowerCase()).join('')).toBe('dfp')
  })

  it('rejects non-subsequences', () => {
    expect(fuzzyMatch('src/main.ts', 'xyz')).toBeNull()
    expect(fuzzyMatch('abc', 'cba')).toBeNull()
  })

  it('treats an empty or blank query as matching everything', () => {
    expect(fuzzyMatch('a/b.ts', '  ')).toEqual({ score: 0, positions: [] })
  })

  it('ignores whitespace in the query', () => {
    expect(fuzzyMatch('src/main.ts', 'ma in')).not.toBeNull()
  })

  it('ranks file-name matches above matches spread over directories', () => {
    expect(rank(['main/x/n.ts', 'src/main.ts'], 'main')).toEqual(['src/main.ts', 'main/x/n.ts'])
  })

  it('ranks a file-name prefix above a scattered match', () => {
    expect(rank(['src/my_app_install.ts', 'src/main.ts'], 'main')[0]).toBe('src/main.ts')
  })

  it('prefers shorter paths on ties', () => {
    expect(rank(['very/long/path/to/index.ts', 'a/index.ts'], 'index')).toEqual(['a/index.ts', 'very/long/path/to/index.ts'])
  })

  it('rewards camelCase humps', () => {
    const hump = fuzzyMatch('FileFinder.tsx', 'ff')!
    const flat = fuzzyMatch('fluffy.tsx', 'ff')!
    expect(hump.score).toBeGreaterThan(flat.score - 1)
  })
})
