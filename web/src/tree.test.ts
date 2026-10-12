import { describe, expect, it } from 'vitest'
import type { ChangedFile } from './api'
import { GENERATED_KEY, ancestorKeys, buildTree, fileLabel } from './tree'

function f(path: string, extra: Partial<ChangedFile> = {}): ChangedFile {
  return { path, status: 'M', oldOid: 'a', newOid: 'b', oldMode: '100644', newMode: '100644', binary: false, generated: false, ...extra }
}

const rowsOf = (files: ChangedFile[], collapsed: string[] = []) =>
  buildTree(files, new Set(collapsed)).rows.map((r) => (r.kind === 'file' ? `${'  '.repeat(r.depth)}${r.file.path}` : `${'  '.repeat(r.depth)}[${r.name}]`))

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

describe('fileLabel', () => {
  it('shows the old base name for a rename in the same directory', () => {
    expect(fileLabel(f('src/Button.tsx', { oldPath: 'src/Btn.tsx' }))).toEqual({ name: 'Button.tsx', renamedFrom: 'Btn.tsx' })
  })

  it('shows the full old path for a move', () => {
    expect(fileLabel(f('lib/a.ts', { oldPath: 'src/a.ts' }))).toEqual({ name: 'a.ts', renamedFrom: 'src/a.ts' })
  })
})
