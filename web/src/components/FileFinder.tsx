import { useEffect, useMemo, useRef, useState } from 'react'
import type { ChangedFile } from '../api'
import { fuzzyMatch } from '../fuzzy'

const MAX_RESULTS = 100

interface Props {
  files: ChangedFile[]
  onPick(path: string): void
  onClose(): void
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

  const results = useMemo(() => {
    const scored: Array<{ file: ChangedFile; score: number; positions: Set<number>; index: number }> = []
    files.forEach((file, index) => {
      const m = fuzzyMatch(file.path, query)
      if (m) scored.push({ file, score: m.score, positions: new Set(m.positions), index })
    })
    if (query.trim()) scored.sort((a, b) => b.score - a.score || a.index - b.index)
    return scored.slice(0, MAX_RESULTS)
  }, [files, query])

  useEffect(() => setActive(0), [query])

  useEffect(() => {
    listRef.current?.querySelector('.is-active')?.scrollIntoView({ block: 'nearest' })
  }, [active])

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setActive((i) => Math.min(i + 1, results.length - 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setActive((i) => Math.max(i - 1, 0))
    } else if (e.key === 'Enter') {
      e.preventDefault()
      const r = results[active]
      if (r) onPick(r.file.path)
    } else if (e.key === 'Escape') {
      e.preventDefault()
      onClose()
    }
  }

  return (
    <div className="finder-backdrop" onMouseDown={onClose}>
      <div className="finder" onMouseDown={(e) => e.stopPropagation()} role="dialog" aria-label="查找文件">
        <div className="finder-title">
          <span>查找文件</span>
          <span className="muted">↑↓ 选择 · Enter 打开 · Esc 关闭</span>
        </div>
        <input
          className="finder-input"
          autoFocus
          spellCheck={false}
          placeholder="输入文件名…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onKeyDown}
        />
        <div className="finder-list" ref={listRef}>
          {results.length === 0 && <div className="finder-empty muted">没有匹配的文件</div>}
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
                <span className={`status-letter st-${r.file.status}`}>{r.file.status}</span>
                <span className={`finder-name st-${r.file.status}`}>
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
