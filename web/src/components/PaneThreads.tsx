import { useState } from 'react'
import type { ReviewThread } from '../api'
import type { PanelItem, PanelReason } from '../threads'

interface Props {
  items: readonly PanelItem[]
  renderThread(thread: ReviewThread, reason: PanelReason): React.ReactNode
  /** Shown above the threads, e.g. that they were read at another commit. */
  notice?: React.ReactNode
}

/**
 * Above the diff: the file's comments that have no line in it (file-level, outdated, or
 * unplaceable, each card saying why). Shown for every kind of file.
 */
export function PaneThreads({ items, renderThread, notice }: Props) {
  const [open, setOpen] = useState(true)
  if (items.length === 0 && !notice) return null
  const files = items.filter((i) => i.reason === 'file').length
  const others = items.length - files
  const summary = [
    files > 0 && `${files} on the file`,
    others > 0 && `${others} not shown in the diff`,
  ]
    .filter(Boolean)
    .join(', ')
  return (
    <div className="pane-threads" data-testid="pane-threads">
      {items.length > 0 && (
        <button type="button" className="pane-threads-head" onClick={() => setOpen(!open)} aria-expanded={open}>
          <span className="thread-caret" aria-hidden>
            {open ? '▾' : '▸'}
          </span>
          Comments: {summary}
        </button>
      )}
      {notice}
      {open && items.length > 0 && (
        <div className="pane-threads-list">
          {items.map((i) => (
            <div key={i.thread.id} className="pane-thread">
              {renderThread(i.thread, i.reason)}
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
