// Changed-file tree: directory grouping with JetBrains-style compression of
// single-child directory chains, plus a separate "Generated" group.
import type { ChangedFile } from './api'

interface DirNode {
  name: string
  key: string
  dirs: Map<string, DirNode>
  files: ChangedFile[]
}

export type TreeRow =
  | { kind: 'dir'; key: string; name: string; depth: number; expanded: boolean; fileCount: number }
  | { kind: 'group'; key: string; name: string; depth: number; expanded: boolean; fileCount: number }
  | { kind: 'file'; key: string; file: ChangedFile; depth: number }

export const GENERATED_KEY = '::generated'

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' })

function baseName(path: string): string {
  return path.slice(path.lastIndexOf('/') + 1)
}

function buildDir(files: ChangedFile[], keyPrefix: string): DirNode {
  const root: DirNode = { name: '', key: keyPrefix, dirs: new Map(), files: [] }
  for (const f of files) {
    const parts = f.path.split('/')
    let node = root
    for (let i = 0; i < parts.length - 1; i++) {
      let next = node.dirs.get(parts[i])
      if (!next) {
        next = { name: parts[i], key: `${node.key}/${parts[i]}`, dirs: new Map(), files: [] }
        node.dirs.set(parts[i], next)
      }
      node = next
    }
    node.files.push(f)
  }
  return root
}

/** Merge `a/` → `b/` while a directory holds exactly one subdirectory and no files. */
function compress(node: DirNode): DirNode {
  let cur = node
  let name = node.name
  while (cur.files.length === 0 && cur.dirs.size === 1) {
    const only = cur.dirs.values().next().value as DirNode
    name = `${name}/${only.name}`
    cur = only
  }
  return name === cur.name ? cur : { ...cur, name }
}

function countFiles(node: DirNode): number {
  let n = node.files.length
  for (const d of node.dirs.values()) n += countFiles(d)
  return n
}

function sortedDirs(node: DirNode): DirNode[] {
  return [...node.dirs.values()].map(compress).sort((a, b) => collator.compare(a.name, b.name))
}

function sortedFiles(node: DirNode): ChangedFile[] {
  return [...node.files].sort((a, b) => collator.compare(baseName(a.path), baseName(b.path)))
}

function emit(node: DirNode, depth: number, collapsed: Set<string>, rows: TreeRow[] | null, order: ChangedFile[]): void {
  for (const d of sortedDirs(node)) {
    const expanded = !collapsed.has(d.key)
    rows?.push({ kind: 'dir', key: d.key, name: d.name, depth, expanded, fileCount: countFiles(d) })
    emit(d, depth + 1, collapsed, expanded ? rows : null, order)
  }
  for (const f of sortedFiles(node)) {
    rows?.push({ kind: 'file', key: f.path, file: f, depth })
    order.push(f)
  }
}

export interface FileTree {
  rows: TreeRow[]
  /** Every file in display order, regardless of collapsed state (used for next/prev file). */
  order: ChangedFile[]
}

export function buildTree(files: ChangedFile[], collapsed: Set<string>): FileTree {
  const rows: TreeRow[] = []
  const order: ChangedFile[] = []
  const main = files.filter((f) => !f.generated)
  const generated = files.filter((f) => f.generated)
  emit(buildDir(main, ''), 0, collapsed, rows, order)
  if (generated.length > 0) {
    const expanded = !collapsed.has(GENERATED_KEY)
    rows.push({ kind: 'group', key: GENERATED_KEY, name: 'Generated', depth: 0, expanded, fileCount: generated.length })
    emit(buildDir(generated, GENERATED_KEY), 1, collapsed, expanded ? rows : null, order)
  }
  return { rows, order }
}

/** Keys of the tree nodes that must be expanded for `path` to be visible. */
export function ancestorKeys(file: Pick<ChangedFile, 'path' | 'generated'>): string[] {
  const prefix = file.generated ? GENERATED_KEY : ''
  const keys = file.generated ? [GENERATED_KEY] : []
  const parts = file.path.split('/')
  let key = prefix
  for (let i = 0; i < parts.length - 1; i++) {
    key = `${key}/${parts[i]}`
    keys.push(key)
  }
  return keys
}

export function fileLabel(file: ChangedFile): { name: string; renamedFrom: string | null } {
  const name = baseName(file.path)
  if (!file.oldPath || file.oldPath === file.path) return { name, renamedFrom: null }
  const oldDir = file.oldPath.slice(0, file.oldPath.lastIndexOf('/') + 1)
  const newDir = file.path.slice(0, file.path.lastIndexOf('/') + 1)
  return { name, renamedFrom: oldDir === newDir ? baseName(file.oldPath) : file.oldPath }
}
