// The symbols of file versions: a bounded cache over the server's per-blob lists, and the pure functions that read a
// flat list as nesting, as the scopes around a line, and as the definition under a position.
import { fileSymbols, type FileSymbols, type RepoRef, type SymbolInfo } from '../api'
import { PinnedLru } from '../lru'

const CACHE_SIZE = 100

type Fetcher = (repo: RepoRef, oid: string, path: string) => Promise<FileSymbols>

/**
 * Symbols by repository, blob oid and path, the most recently used `capacity` of them. Callers that ask while a request
 * is under way share it, and a failed request is not kept, so the next call tries again.
 */
export function createSymbolCache(fetcher: Fetcher, capacity: number): Fetcher {
  const failed = new WeakSet<Promise<FileSymbols>>()
  const cache = new PinnedLru<Promise<FileSymbols>>(capacity, () => undefined, (p) => !failed.has(p))
  return (repo, oid, path) => {
    const key = `${repo.host}/${repo.owner}/${repo.repo}\0${oid}\0${path}`
    const hit = cache.get(key)
    if (hit) return hit
    const pending = fetcher(repo, oid, path)
    pending.catch(() => failed.add(pending))
    cache.put(key, pending)
    return pending
  }
}

/** The symbols of one version of a file (`path` picks the grammar); the list is shared, don't change it. */
export const getSymbols = createSymbolCache(fileSymbols, CACHE_SIZE)

/** A symbol and the symbols defined inside it. */
export interface SymbolNode {
  symbol: SymbolInfo
  children: SymbolNode[]
}

/** Outermost first: the earlier start, then the longer definition, then the leftmost name. */
function byExtent(a: SymbolInfo, b: SymbolInfo): number {
  return a.startLine - b.startLine || b.endLine - a.endLine || a.line - b.line || a.column - b.column
}

/**
 * The flat list as a tree: a symbol goes inside the nearest one that spans all its lines. A definition on a single line
 * holds nothing, and one that only partly overlaps another sits beside it, so any input gives one tree, in source order.
 */
export function nestSymbols(symbols: readonly SymbolInfo[]): SymbolNode[] {
  const roots: SymbolNode[] = []
  const open: SymbolNode[] = []
  for (const symbol of [...symbols].sort(byExtent)) {
    while (open.length > 0) {
      const outer = open[open.length - 1].symbol
      if (outer.startLine < outer.endLine && symbol.endLine <= outer.endLine) break
      open.pop()
    }
    const node: SymbolNode = { symbol, children: [] }
    const siblings = open.length > 0 ? open[open.length - 1].children : roots
    siblings.push(node)
    open.push(node)
  }
  return roots
}

/**
 * The symbols whose body holds `line`, outermost first. A symbol's own first line is not inside it: when that line is
 * the one at the top of the editor, its header is already in view.
 */
export function enclosingScopes(symbols: readonly SymbolInfo[], line: number): SymbolInfo[] {
  return symbols.filter((s) => s.startLine < line && line <= s.endLine).sort(byExtent)
}

/** The symbol whose name covers the position (`column` is 1-based, and the name's end is not part of it). */
export function definitionAt(symbols: readonly SymbolInfo[], line: number, column: number): SymbolInfo | undefined {
  return symbols.find((s) => s.line === line && column >= s.column && column < s.endColumn)
}
