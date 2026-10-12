import { useDeferredValue, useEffect, useMemo, useRef, useState } from 'react'
import type { FileStatus } from '../api'
import { fuzzyMatch } from '../fuzzy'

const MAX_RESULTS = 100

/** A changed file carries its status; an unchanged one (full project tree) has none. */
interface FinderFile {
  path: string
  status?: FileStatus
}

interface Props {
  files: readonly FinderFile[]
  onPick(path: string): void
  onClose(): void
}

type Result = { file: FinderFile; score: number; positions: Set<number>; index: number }

function rank(files: readonly FinderFile[], query: string): Result[] {
  const scored: Result[] = []
  files.forEach((file, index) => {
    const m = fuzzyMatch(file.path, query)
    if (m) scored.push({ file, score: m.score, positions: new Set(m.positions), index })
  })
  if (query.trim()) scored.sort((a, b) => b.score - a.score || a.index - b.index)
  return scored.slice(0, MAX_RESULTS)
}

function Highlighted({ text, positions, offset }: { text: string; positions: Set<number>; offset: number }) {
  const parts: React.ReactNode[] = []
  for (let i = 0; i < text.length; i++) {
    const ch = text[i]
    parts.push(positions.has(i + offset) ? <mark key={i}>{ch}</mark> : ch)
  }
  return <>{parts}</>
}

export function FileFinder({ files, onPick, onClose }: Props) {
  const [query, setQuery] = useState('')
  const [active, setActive] = useState(0)
  const listRef = useRef<HTMLDivElement>(null)
  // Matching a whole project tree takes a while: keep typing responsive.
  const deferredQuery = useDeferredValue(query)

  const results = useMemo(() => rank(files, deferredQuery), [files, deferredQuery])
  // The list on screen can lag behind the input; keys act on it only once it caught up.
  const settled = query === deferredQuery

  useEffect(() => setActive(0), [deferredQuery])

  useEffect(() => {
    listRef.current?.querySelector('.is-active')?.scrollIntoView({ block: 'nearest' })
  }, [active])

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      if (settled) setActive((i) => Math.min(i + 1, results.length - 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      if (settled) setActive((i) => Math.max(i - 1, 0))
    } else if (e.key === 'Enter') {
      e.preventDefault()
      // Typed ahead of the list: the best match for what is in the input, not a stale row.
      const r = settled ? results[active] : rank(files, query)[0]
      if (r) onPick(r.file.path)
    } else if (e.key === 'Escape') {
      e.preventDefault()
      onClose()
    }
  }

  return (
    <div className="finder-backdrop" onMouseDown={onClose}>
      <div className="finder" onMouseDown={(e) => e.stopPropagation()} role="dialog" aria-label="Find file">
        <div className="finder-title">
          <span>Find file</span>
          <span className="muted">↑↓ select · Enter open · Esc close</span>
        </div>
        <input
          className="finder-input"
          autoFocus
          spellCheck={false}
          placeholder="Type a file name…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onKeyDown}
        />
        <div className="finder-list" ref={listRef}>
          {results.length === 0 && <div className="finder-empty muted">No matching files</div>}
          {results.map((r, i) => {
            const p = r.file.path
            const slash = p.lastIndexOf('/') + 1
            return (
              <div
                key={p}
                className={`finder-item${i === active ? ' is-active' : ''}`}
                onMouseMove={() => setActive(i)}
                onClick={() => onPick(p)}
              >
                <span className={`status-letter${r.file.status ? ` st-${r.file.status}` : ''}`}>{r.file.status}</span>
                <span className={`finder-name${r.file.status ? ` st-${r.file.status}` : ''}`}>
                  <Highlighted text={p.slice(slash)} positions={r.positions} offset={slash} />
                </span>
                <span className="finder-dir muted">
                  <Highlighted text={p.slice(0, slash)} positions={r.positions} offset={0} />
                </span>
              </div>
            )
          })}
        </div>
      </div>
    </div>
  )
}
