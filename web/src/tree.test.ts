import { describe, expect, it } from 'vitest'
import type { ChangedFile, TreeEntry } from './api'
import {
  GENERATED_KEY,
  adjacentChanged,
  ancestorKeys,
  defaultExpanded,
  fileLabel,
  isChanged,
  prepareFullTree,
  prepareTree,
  treeRows,
  type TreeRow,
} from './tree'

function f(path: string, extra: Partial<ChangedFile> = {}): ChangedFile {
  return { path, status: 'M', oldOid: 'a', newOid: 'b', oldMode: '100644', newMode: '100644', binary: false, generated: false, ...extra }
}

function e(path: string, mode = '100644'): TreeEntry {
  return { path, mode, oid: 'c' }
}

/** The changed-files tree with everything expanded except `collapsed`, as PrPage builds it. */
function buildTree(files: ChangedFile[], collapsed: Set<string>) {
  const tree = prepareTree(files)
  return { rows: treeRows(tree, (k) => !collapsed.has(k)), order: tree.all.filter(isChanged) }
}

function show(r: TreeRow): string {
  const indent = '  '.repeat(r.depth)
  if (r.kind === 'file') return `${indent}${r.file.status} ${r.file.path}`
  if (r.kind === 'entry') return `${indent}${r.entry.path}`
  return `${indent}[${r.name}]`
}

const rowsOf = (files: ChangedFile[], collapsed: string[] = []) =>
  buildTree(files, new Set(collapsed)).rows.map((r) => show(r).replace(/^(\s*)M /, '$1'))

describe('buildTree', () => {
  it('compresses single-child directory chains', () => {
    expect(rowsOf([f('src/server/api/v1/user.rs'), f('src/server/api/v1/post.rs')])).toEqual([
      '[src/server/api/v1]',
      '  src/server/api/v1/post.rs',
      '  src/server/api/v1/user.rs',
    ])
  })

  it('stops compressing at a directory that has files or several subdirectories', () => {
    expect(rowsOf([f('a/b/x.ts'), f('a/b/c/y.ts'), f('a/b/d/z.ts')])).toEqual([
      '[a/b]',
      '  [c]',
      '    a/b/c/y.ts',
      '  [d]',
      '    a/b/d/z.ts',
      '  a/b/x.ts',
    ])
  })

  it('sorts directories before files, naturally', () => {
    expect(rowsOf([f('z.ts'), f('file10.ts'), f('file2.ts'), f('lib/a.ts')])).toEqual([
      '[lib]',
      '  lib/a.ts',
      'file2.ts',
      'file10.ts',
      'z.ts',
    ])
  })

  it('puts generated files in their own group after the rest', () => {
    const files = [f('pnpm-lock.yaml', { generated: true }), f('src/a.ts'), f('dist/x.js', { generated: true })]
    expect(rowsOf(files)).toEqual(['[src]', '  src/a.ts', '[Generated]', '  [dist]', '    dist/x.js', '  pnpm-lock.yaml'])
    expect(rowsOf(files, [GENERATED_KEY])).toEqual(['[src]', '  src/a.ts', '[Generated]'])
  })

  it('hides collapsed directories from rows but keeps every file in order', () => {
    const files = [f('a/one.ts'), f('b/two.ts'), f('three.ts')]
    const tree = buildTree(files, new Set(['/a']))
    expect(tree.rows.map((r) => r.key)).toEqual(['/a', '/b', 'b/two.ts', 'three.ts'])
    expect(tree.order.map((x) => x.path)).toEqual(['a/one.ts', 'b/two.ts', 'three.ts'])
    const dir = tree.rows[0]
    expect(dir.kind === 'dir' && !dir.expanded && dir.fileCount).toBe(1)
  })

  it('uses the full compressed key so collapsing works on compressed rows', () => {
    const files = [f('x/y/z/1.ts')]
    const keys = buildTree(files, new Set()).rows.map((r) => r.key)
    expect(keys[0]).toBe('/x/y/z')
    expect(buildTree(files, new Set(['/x/y/z'])).rows).toHaveLength(1)
  })
})

describe('ancestorKeys', () => {
  it('lists every directory key on the way to the file', () => {
    expect(ancestorKeys(f('a/b/c.ts'))).toEqual(['/a', '/a/b'])
    expect(ancestorKeys(f('top.ts'))).toEqual([])
  })

  it('includes the generated group for generated files', () => {
    expect(ancestorKeys(f('dist/x.js', { generated: true }))).toEqual([GENERATED_KEY, `${GENERATED_KEY}/dist`])
  })

  it('matches the keys buildTree produces', () => {
    const file = f('a/b/c/d.ts')
    const sibling = f('a/b/e.ts')
    const dirKeys = buildTree([file, sibling], new Set()).rows.filter((r) => r.kind !== 'file').map((r) => r.key)
    for (const k of dirKeys) expect(ancestorKeys(file)).toContain(k)
  })
})

describe('prepareTree', () => {
  it('counts every file of a directory and emits only expanded ones', () => {
    const tree = prepareTree([f('a/x.ts'), f('a/b/y.ts'), f('c.ts')])
    expect(tree.all.map((x) => x.path)).toEqual(['a/b/y.ts', 'a/x.ts', 'c.ts'])
    const rows = treeRows(tree, (k) => k !== '/a/b')
    expect(rows.map(show)).toEqual(['[a]', '  [b]', '  M a/x.ts', 'M c.ts'])
    expect(rows[0].kind === 'dir' && [rows[0].fileCount, rows[0].changedCount]).toEqual([2, 2])
  })
})

describe('prepareFullTree', () => {
  const changed = [
    f('src/app.ts'),
    f('src/new.ts', { status: 'A', oldOid: null }),
    f('src/old.ts', { status: 'D', newOid: null }),
    f('pnpm-lock.yaml', { generated: true }),
  ]
  const entries = [e('README.md'), e('src/app.ts'), e('src/new.ts'), e('src/util/str.ts'), e('docs/a/b.md'), e('pnpm-lock.yaml'), e('vendor/lib', '160000')]
  const tree = prepareFullTree(changed, entries)
  const all = (k: string) => k !== 'none'

  it('shows the head tree with changed files in place and deletions at their old path', () => {
    expect(treeRows(tree, all).map(show)).toEqual([
      '[docs/a]',
      '  docs/a/b.md',
      '[src]',
      '  [util]',
      '    src/util/str.ts',
      '  M src/app.ts',
      '  A src/new.ts',
      '  D src/old.ts',
      '[vendor]',
      '  vendor/lib',
      'M pnpm-lock.yaml',
      'README.md',
    ])
    expect(tree.all.filter(isChanged).map((x) => x.path)).toEqual(['src/app.ts', 'src/new.ts', 'src/old.ts', 'pnpm-lock.yaml'])
  })

  it('counts changed files per directory', () => {
    const dirs = treeRows(tree, all).flatMap((r) => (r.kind === 'dir' ? [[r.name, r.fileCount, r.changedCount]] : []))
    expect(dirs).toEqual([
      ['docs/a', 1, 0],
      ['src', 4, 3],
      ['util', 1, 0],
      ['vendor', 1, 0],
    ])
  })

  it('opens only the directories that lead to a changed file by default', () => {
    const open = defaultExpanded(changed)
    expect([...open].sort()).toEqual(['/src'])
    expect(treeRows(tree, (k) => open.has(k)).map(show)).toEqual([
      '[docs/a]',
      '[src]',
      '  [util]',
      '  M src/app.ts',
      '  A src/new.ts',
      '  D src/old.ts',
      '[vendor]',
      'M pnpm-lock.yaml',
      'README.md',
    ])
  })

  it('uses the same keys as ancestorKeys, so revealing a file opens its folders', () => {
    const keys = ancestorKeys({ path: 'docs/a/b.md' })
    const rows = treeRows(tree, (k) => keys.includes(k))
    expect(rows.map(show)).toContain('  docs/a/b.md')
  })
})

describe('large trees', () => {
  it('handles a directory with more files than the engine accepts as call arguments', () => {
    const entries = Array.from({ length: 200_000 }, (_, i) => e(`big/f${i}.txt`))
    const deleted = Array.from({ length: 200_000 }, (_, i) => f(`gone/d${i}.txt`, { status: 'D', newOid: null }))
    const tree = prepareFullTree(deleted, entries)
    expect(tree.all).toHaveLength(400_000)
  })
})

describe('adjacentChanged', () => {
  const all = prepareFullTree([f('b/x.ts'), f('d.ts')], [e('a/one.ts'), e('b/x.ts'), e('c.ts'), e('d.ts'), e('e.ts')]).all

  it('finds the nearest changed file in either direction', () => {
    expect(all.map((x) => x.path)).toEqual(['a/one.ts', 'b/x.ts', 'c.ts', 'd.ts', 'e.ts'])
    expect(adjacentChanged(all, 'c.ts', 1)?.path).toBe('d.ts')
    expect(adjacentChanged(all, 'c.ts', -1)?.path).toBe('b/x.ts')
    expect(adjacentChanged(all, 'a/one.ts', -1)).toBeNull()
    expect(adjacentChanged(all, 'e.ts', 1)).toBeNull()
    expect(adjacentChanged(all, 'missing', 1)).toBeNull()
  })
})

describe('fileLabel', () => {
  it('shows the old base name for a rename in the same directory', () => {
    expect(fileLabel(f('src/Button.tsx', { oldPath: 'src/Btn.tsx' }))).toEqual({ name: 'Button.tsx', renamedFrom: 'Btn.tsx' })
  })

  it('shows the full old path for a move', () => {
    expect(fileLabel(f('lib/a.ts', { oldPath: 'src/a.ts' }))).toEqual({ name: 'a.ts', renamedFrom: 'src/a.ts' })
  })
})
