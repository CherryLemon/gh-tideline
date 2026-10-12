import { useEffect, useRef } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import type { ChangedFile, ViewedState } from '../api'
import { fileLabel, type TreeRow } from '../tree'

/** Which files the tree lists: the changed ones, or the whole head tree. */
export type TreeScope = 'changed' | 'all'

/**
 * Progress of the head tree listing the `all` scope needs; null once it is there. `stale`:
 * the server has moved on to a newer head, so only a refresh helps.
 */
export type ListingState = { kind: 'loading' } | { kind: 'error'; message: string; stale: boolean } | null

const ROW_HEIGHT = 22
const INDENT = 14

interface Props {
  rows: TreeRow[]
  /** The changed files (header counts). */
  files: ChangedFile[]
  scope: TreeScope
  onScopeChange(scope: TreeScope): void
  /** Files in the head tree, once listed. */
  totalFiles: number | null
  listing: ListingState
  onRetryListing(): void
  currentPath: string | null
  /** Viewed marks of the paths GitHub lists; other files get no checkbox. */
  viewed: ReadonlyMap<string, ViewedState>
  onOpen(path: string): void
  onToggle(key: string): void
  onToggleViewed(path: string, viewed: boolean): void
}

function Chevron({ expanded }: { expanded: boolean }) {
  return (
    <svg className={`chevron${expanded ? ' is-open' : ''}`} viewBox="0 0 16 16" aria-hidden>
      <path d="M6 4l4 4-4 4" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  )
}

function FolderIcon() {
  return (
    <svg className="folder-icon" viewBox="0 0 16 16" aria-hidden>
      <path d="M1.5 3.5h4.2l1.5 1.5h7.3v7.5a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1z" fill="#F6D7B0" stroke="#D9B98C" strokeWidth="1" />
    </svg>
  )
}

function Counts({ file }: { file: ChangedFile }) {
  if (file.binary) return <span className="counts muted">bin</span>
  const a = file.additions ?? 0
  const d = file.deletions ?? 0
  return (
    <span className="counts">
      {a > 0 && <span className="add">+{a}</span>}
      {d > 0 && <span className="del">-{d}</span>}
    </span>
  )
}

function ViewedProgress({ viewed }: { viewed: ReadonlyMap<string, ViewedState> }) {
  if (viewed.size === 0) return null
  let done = 0
  for (const state of viewed.values()) if (state === 'VIEWED') done++
  return (
    <div className="tree-footer" title="GitHub 上的已看状态，包括生成的文件">
      <span>
        已看 {done}/{viewed.size}
      </span>
      <div className="progress">
        <div className="progress-bar" style={{ width: `${(done / viewed.size) * 100}%` }} />
      </div>
    </div>
  )
}

function baseName(path: string): string {
  return path.slice(path.lastIndexOf('/') + 1)
}

export function FileTree({
  rows,
  files,
  scope,
  onScopeChange,
  totalFiles,
  listing,
  onRetryListing,
  currentPath,
  viewed,
  onOpen,
  onToggle,
  onToggleViewed,
}: Props) {
  const scrollRef = useRef<HTMLDivElement>(null)
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 16,
  })

  const selectedIndex = rows.findIndex((r) => (r.kind === 'file' || r.kind === 'entry') && r.key === currentPath)
  useEffect(() => {
    if (selectedIndex >= 0) virtualizer.scrollToIndex(selectedIndex, { align: 'auto' })
  }, [selectedIndex, virtualizer])

  const additions = files.reduce((n, f) => n + (f.additions ?? 0), 0)
  const deletions = files.reduce((n, f) => n + (f.deletions ?? 0), 0)

  return (
    <div className="tree">
      <div className="tree-header">
        <div className="segmented tree-scope" role="group" aria-label="文件树范围">
          <button className={scope === 'changed' ? 'is-on' : ''} aria-pressed={scope === 'changed'} onClick={() => onScopeChange('changed')} title="只列出改动的文件">
            改动 <span className="seg-count">{files.length}</span>
          </button>
          <button className={scope === 'all' ? 'is-on' : ''} aria-pressed={scope === 'all'} onClick={() => onScopeChange('all')} title="列出 head 上的整个项目">
            全部{totalFiles !== null && <span className="seg-count"> {totalFiles}</span>}
          </button>
        </div>
        <span className="add">+{additions}</span>
        <span className="del">-{deletions}</span>
      </div>
      {listing && (
        <div className="tree-listing">
          {listing.kind === 'loading' ? (
            <span className="muted">正在列出项目文件…</span>
          ) : (
            <>
              <span className="pane-error">{listing.message}</span>
              <button className="btn" onClick={onRetryListing}>
                {listing.stale ? '刷新' : '重试'}
              </button>
            </>
          )}
        </div>
      )}
      <div className="tree-scroll" ref={scrollRef} role="tree">
        <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
          {virtualizer.getVirtualItems().map((item) => {
            const row = rows[item.index]
            const style = {
              transform: `translateY(${item.start}px)`,
              height: ROW_HEIGHT,
              paddingLeft: 6 + row.depth * INDENT,
            }
            if (row.kind === 'file') {
              const f = row.file
              const { name, renamedFrom } = fileLabel(f)
              const selected = f.path === currentPath
              const mark = viewed.get(f.path)
              return (
                <div
                  key={row.key}
                  className={`tree-row tree-file${selected ? ' is-selected' : ''}${mark === 'VIEWED' ? ' is-viewed' : ''}${mark === 'DISMISSED' ? ' is-dismissed' : ''}`}
                  style={style}
                  role="treeitem"
                  aria-selected={selected}
                  title={(renamedFrom ? `${f.oldPath} → ${f.path}` : f.path) + (mark === 'DISMISSED' ? '\n看过之后又被修改' : '')}
                  data-path={f.path}
                  onClick={() => onOpen(f.path)}
                >
                  <span className={`status-letter st-${f.status}`}>{f.status}</span>
                  <span className={`file-name st-${f.status}`}>
                    {renamedFrom && (
                      <>
                        <span className="rename-from">{renamedFrom}</span>
                        <span className="arrow"> → </span>
                      </>
                    )}
                    {name}
                  </span>
                  <Counts file={f} />
                  {mark !== undefined && (
                    <input
                      type="checkbox"
                      className="viewed-box"
                      checked={mark === 'VIEWED'}
                      title={mark === 'VIEWED' ? '已看' : '标记为已看'}
                      aria-label={`已看 ${f.path}`}
                      onClick={(e) => e.stopPropagation()}
                      onChange={(e) => onToggleViewed(f.path, e.target.checked)}
                    />
                  )}
                </div>
              )
            }
            if (row.kind === 'entry') {
              const selected = row.key === currentPath
              return (
                <div
                  key={row.key}
                  className={`tree-row tree-file is-unchanged${selected ? ' is-selected' : ''}`}
                  style={style}
                  role="treeitem"
                  aria-selected={selected}
                  title={`${row.entry.path}\n未改动`}
                  data-path={row.entry.path}
                  onClick={() => onOpen(row.entry.path)}
                >
                  <span className="status-letter" />
                  <span className="file-name">{baseName(row.entry.path)}</span>
                </div>
              )
            }
            // The full tree counts changed files per folder; the changed tree counts all of them.
            const full = scope === 'all'
            const count = full ? row.changedCount : row.fileCount
            return (
              <div
                key={row.key}
                className={`tree-row tree-dir${row.kind === 'group' ? ' tree-group' : ''}${full && row.changedCount > 0 ? ' has-changes' : ''}`}
                style={style}
                role="treeitem"
                aria-expanded={row.expanded}
                data-key={row.key}
                onClick={() => onToggle(row.key)}
              >
                <Chevron expanded={row.expanded} />
                {row.kind === 'dir' && <FolderIcon />}
                <span className="dir-name">{row.name}</span>
                {count > 0 && (
                  <span className="dir-count muted" title={full ? `${count} 个改动文件` : undefined}>
                    {count}
                  </span>
                )}
              </div>
            )
          })}
        </div>
      </div>
      <ViewedProgress viewed={viewed} />
    </div>
  )
}
