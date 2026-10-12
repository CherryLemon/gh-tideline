// File tree: directory grouping with JetBrains-style compression of single-child
// directory chains. Two shapes: the changed files (with a separate "Generated" group),
// or the whole head tree with the changed files in place (`prepareFullTree`).
// The structure is prepared once per file list; rows are emitted per expanded state.
import type { ChangedFile, TreeEntry } from './api'

/** A changed file, or an unchanged file of the head tree. */
export type TreeFile = ChangedFile | TreeEntry

export function isChanged(f: TreeFile): f is ChangedFile {
  return 'status' in f
}

interface DirNode {
  name: string
  key: string
  /** Sorted and compressed. */
  dirs: DirNode[]
  /** Sorted by base name. */
  files: TreeFile[]
  fileCount: number
  changedCount: number
}

export type TreeRow =
  | { kind: 'dir'; key: string; name: string; depth: number; expanded: boolean; fileCount: number; changedCount: number }
  | { kind: 'group'; key: string; name: string; depth: number; expanded: boolean; fileCount: number; changedCount: number }
  | { kind: 'file'; key: string; file: ChangedFile; depth: number }
  | { kind: 'entry'; key: string; entry: TreeEntry; depth: number }

export const GENERATED_KEY = '::generated'

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' })

function baseName(path: string): string {
  return path.slice(path.lastIndexOf('/') + 1)
}

interface RawDir {
  name: string
  key: string
  dirs: Map<string, RawDir>
  files: TreeFile[]
}

function rawTree(files: readonly TreeFile[], keyPrefix: string): RawDir {
  const root: RawDir = { name: '', key: keyPrefix, dirs: new Map(), files: [] }
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

/**
 * Sorts, counts and compresses: `a/` → `b/` merge while a directory holds exactly one
 * subdirectory and no files. A merged node takes the key of its deepest directory.
 * The root itself is never compressed (its children are the top-level rows).
 */
function finish(raw: RawDir, isRoot = false): DirNode {
  let cur = raw
  let name = raw.name
  while (!isRoot && cur.files.length === 0 && cur.dirs.size === 1) {
    cur = cur.dirs.values().next().value as RawDir
    name = `${name}/${cur.name}`
  }
  const dirs = [...cur.dirs.values()].map((d) => finish(d)).sort((a, b) => collator.compare(a.name, b.name))
  const files = [...cur.files].sort((a, b) => collator.compare(baseName(a.path), baseName(b.path)))
  let fileCount = files.length
  let changedCount = files.filter(isChanged).length
  for (const d of dirs) {
    fileCount += d.fileCount
    changedCount += d.changedCount
  }
  return { name, key: cur.key, dirs, files, fileCount, changedCount }
}

interface Section {
  /** A collapsible group row above the section ("Generated"), or none. */
  group: { key: string; name: string } | null
  root: DirNode
}

export interface PreparedTree {
  sections: Section[]
  /** Every file in display order, regardless of expanded state. */
  all: TreeFile[]
}

// Pushed one by one: spreading a 100k-element array into push() overflows the stack.
function collect(node: DirNode, out: TreeFile[]): void {
  for (const d of node.dirs) collect(d, out)
  for (const f of node.files) out.push(f)
}

function prepared(sections: Section[]): PreparedTree {
  const all: TreeFile[] = []
  for (const s of sections) collect(s.root, all)
  return { sections, all }
}

/** The changed files, generated ones in their own group after the rest. */
export function prepareTree(files: readonly ChangedFile[]): PreparedTree {
  const main = files.filter((f) => !f.generated)
  const generated = files.filter((f) => f.generated)
  const sections: Section[] = [{ group: null, root: finish(rawTree(main, ''), true) }]
  if (generated.length > 0) {
    sections.push({ group: { key: GENERATED_KEY, name: 'Generated' }, root: finish(rawTree(generated, GENERATED_KEY), true) })
  }
  return prepared(sections)
}

/**
 * The whole head tree (`entries`) with each changed file in place of its entry. Deleted
 * files are not in the head tree and are added at their old location. Generated files
 * stay where they are.
 */
export function prepareFullTree(changed: readonly ChangedFile[], entries: readonly TreeEntry[]): PreparedTree {
  const byPath = new Map(changed.map((f) => [f.path, f]))
  const files: TreeFile[] = []
  for (const e of entries) {
    const c = byPath.get(e.path)
    files.push(c ?? e)
    byPath.delete(e.path)
  }
  // Deletions, plus anything the head tree lacks for another reason.
  for (const f of byPath.values()) files.push(f)
  return prepared([{ group: null, root: finish(rawTree(files, ''), true) }])
}

function emit(node: DirNode, depth: number, isExpanded: (key: string) => boolean, rows: TreeRow[]): void {
  for (const d of node.dirs) {
    const expanded = isExpanded(d.key)
    rows.push({ kind: 'dir', key: d.key, name: d.name, depth, expanded, fileCount: d.fileCount, changedCount: d.changedCount })
    if (expanded) emit(d, depth + 1, isExpanded, rows)
  }
  for (const f of node.files) {
    if (isChanged(f)) rows.push({ kind: 'file', key: f.path, file: f, depth })
    else rows.push({ kind: 'entry', key: f.path, entry: f, depth })
  }
}

/** Visible rows: only expanded directories are descended into. */
export function treeRows(tree: PreparedTree, isExpanded: (key: string) => boolean): TreeRow[] {
  const rows: TreeRow[] = []
  for (const { group, root } of tree.sections) {
    if (!group) {
      emit(root, 0, isExpanded, rows)
      continue
    }
    const expanded = isExpanded(group.key)
    rows.push({ kind: 'group', key: group.key, name: group.name, depth: 0, expanded, fileCount: root.fileCount, changedCount: root.changedCount })
    if (expanded) emit(root, 1, isExpanded, rows)
  }
  return rows
}

/** Keys of the tree nodes that must be expanded for `path` to be visible. */
export function ancestorKeys(file: { path: string; generated?: boolean }): string[] {
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

/** Directories of the full tree to open at first: those leading to a changed file. */
export function defaultExpanded(changed: readonly ChangedFile[]): Set<string> {
  const keys = new Set<string>()
  for (const f of changed) for (const k of ancestorKeys({ path: f.path })) keys.add(k)
  return keys
}

/**
 * The nearest changed file after (`dir` 1) or before (-1) `path` in display order; for
 * stepping from an unchanged file. Null if there is none or `path` is not listed.
 */
export function adjacentChanged(all: readonly TreeFile[], path: string, dir: 1 | -1): ChangedFile | null {
  const at = all.findIndex((f) => f.path === path)
  if (at < 0) return null
  for (let i = at + dir; i >= 0 && i < all.length; i += dir) {
    const f = all[i]
    if (isChanged(f)) return f
  }
  return null
}

export function fileLabel(file: ChangedFile): { name: string; renamedFrom: string | null } {
  const name = baseName(file.path)
  if (!file.oldPath || file.oldPath === file.path) return { name, renamedFrom: null }
  const oldDir = file.oldPath.slice(0, file.oldPath.lastIndexOf('/') + 1)
  const newDir = file.path.slice(0, file.path.lastIndexOf('/') + 1)
  return { name, renamedFrom: oldDir === newDir ? baseName(file.oldPath) : file.oldPath }
}
