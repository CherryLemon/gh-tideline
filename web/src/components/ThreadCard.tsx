import { memo, useId, useState } from 'react'
import type { DiffSide, ReviewComment, ReviewThread } from '../api'
import { hunkOf, type LineAnchor, type PanelReason } from '../threads'
import { SafeHtml } from './SafeHtml'

/** Why a thread shows in the panel instead of under its line, as a chip; null: nothing to say. */
const REASON_CHIPS: Record<PanelReason, { label: string; title: string } | null> = {
  file: null,
  outdated: { label: 'Outdated', title: 'The lines this comment was made on have changed since' },
  head: { label: 'Other commit', title: 'The comments were read at another commit than the one shown' },
  'no-diff': null,
  'old-path': { label: 'Old path', title: 'Made on the file under its old name' },
  'out-of-range': { label: 'Not in file', title: 'The line is past the end of the file as shown' },
}

function sideMark(side: DiffSide | null): string {
  return side === 'LEFT' ? '−' : '+'
}

/** "Line +12", "Lines +10 to +12": the lines a new comment is about. */
export function anchorLines(a: LineAnchor): string {
  const end = `${sideMark(a.side)}${a.line}`
  return a.startLine === null || a.startLine === a.line ? `Line ${end}` : `Lines ${sideMark(a.side)}${a.startLine} to ${end}`
}

/** "Line +12", "Lines −5 to +8"; outdated threads give their original lines. */
export function threadLines(t: ReviewThread): string | null {
  if (t.subjectType === 'FILE') return null
  const current = t.line !== null
  const end = current ? t.line : t.originalLine
  const start = current ? t.startLine : t.originalStartLine
  if (end === null) return null
  const endText = `${sideMark(t.side)}${end}`
  if (start === null || (start === end && (t.startSide ?? t.side) === t.side)) return `Line ${endText}`
  return `Lines ${sideMark(t.startSide ?? t.side)}${start} to ${endText}`
}

const RELATIVE = new Intl.RelativeTimeFormat('en', { numeric: 'auto' })
const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ['year', 365 * 24 * 3600],
  ['month', 30 * 24 * 3600],
  ['week', 7 * 24 * 3600],
  ['day', 24 * 3600],
  ['hour', 3600],
  ['minute', 60],
]

function timeAgo(iso: string, now = Date.now()): string {
  const seconds = (new Date(iso).getTime() - now) / 1000
  if (!Number.isFinite(seconds)) return iso
  for (const [unit, size] of UNITS) {
    if (Math.abs(seconds) >= size) return RELATIVE.format(Math.round(seconds / size), unit)
  }
  return 'just now'
}

function httpsUrl(url: string | null | undefined): string | undefined {
  return url && /^https:\/\//i.test(url) ? url : undefined
}

function Avatar({ comment }: { comment: ReviewComment }) {
  const src = httpsUrl(comment.author?.avatarUrl)
  if (src) return <img className="avatar" src={src} alt="" width={20} height={20} />
  const letter = comment.author?.login.slice(0, 1).toUpperCase() ?? '?'
  return (
    <span className="avatar avatar-letter" aria-hidden>
      {letter}
    </span>
  )
}

function Comment({ comment }: { comment: ReviewComment }) {
  const [showHidden, setShowHidden] = useState(false)
  const login = comment.author?.login ?? 'ghost'
  return (
    <div className="comment" data-comment-id={comment.id}>
      <div className="comment-head">
        <Avatar comment={comment} />
        <span className={`comment-author${comment.author ? '' : ' muted'}`} title={comment.author ? undefined : 'Deleted account'}>
          {login}
        </span>
        <a className="comment-time" href={httpsUrl(comment.url)} target="_blank" rel="noopener noreferrer" title={new Date(comment.createdAt).toLocaleString()}>
          {timeAgo(comment.createdAt)}
        </a>
        {comment.lastEditedAt && (
          <span className="muted" title={`Edited ${new Date(comment.lastEditedAt).toLocaleString()}`}>
            edited
          </span>
        )}
        {comment.state === 'PENDING' && (
          <span className="chip chip-pending" title="Part of your review; others see it once you submit the review">
            Pending
          </span>
        )}
      </div>
      {comment.isMinimized && !showHidden ? (
        <div className="comment-hidden">
          <span className="muted">
            This comment was hidden{comment.minimizedReason ? ` (${comment.minimizedReason.toLowerCase().replace(/_/g, ' ')})` : ''}.
          </span>
          <button type="button" className="link-btn" onClick={() => setShowHidden(true)}>
            Show
          </button>
        </div>
      ) : (
        <SafeHtml className="comment-body" html={comment.bodyHtml} />
      )}
    </div>
  )
}

function Hunk({ thread }: { thread: ReviewThread }) {
  const lines = hunkOf(thread)
  if (lines.length === 0) return null
  return (
    <pre className="thread-hunk">
      {lines.map((l, i) => (
        <div key={i} className={`hunk-${l.kind}`}>
          {l.kind === 'add' ? '+' : l.kind === 'del' ? '-' : ' '}
          {l.text}
        </div>
      ))}
    </pre>
  )
}

/** Characters of the first comment shown in a collapsed card's header. */
const PREVIEW_LENGTH = 160

/** What the header button says to assistive tech: where, who, how many, which state. */
function headLabel(thread: ReviewThread, lines: string | null, chip: string | null, pending: boolean): string {
  const first = thread.comments[0]
  const count = thread.comments.length
  const where = thread.subjectType === 'FILE' ? 'Comment on the file' : `Thread${lines ? ` on ${lines.toLowerCase()}` : ''}`
  return [
    `${where}${first ? ` by ${first.author?.login ?? 'ghost'}` : ''}`,
    `${count} ${count === 1 ? 'comment' : 'comments'}`,
    chip?.toLowerCase(),
    pending && 'pending',
    thread.isResolved && 'resolved',
  ]
    .filter(Boolean)
    .join(', ')
}

interface Props {
  thread: ReviewThread
  /** Set when the thread is listed in the panel instead of under its line. */
  reason?: PanelReason
}

/**
 * A review thread: its comments, collapsible; resolved and outdated threads start collapsed.
 * Memoized against re-renders of the page (each caret move); a new read re-renders it.
 */
export const ThreadCard = memo(function ThreadCard({ thread, reason }: Props) {
  const bodyId = useId()
  const [open, setOpen] = useState(!thread.isCollapsed && reason !== 'outdated')
  // Follow GitHub when it collapses or expands the thread (resolving does).
  const [collapsedWas, setCollapsedWas] = useState(thread.isCollapsed)
  if (collapsedWas !== thread.isCollapsed) {
    setCollapsedWas(thread.isCollapsed)
    setOpen(!thread.isCollapsed)
  }
  const first = thread.comments[0]
  const lines = threadLines(thread)
  const chip = reason ? REASON_CHIPS[reason] : null
  const pending = thread.comments.some((c) => c.state === 'PENDING')
  const count = thread.comments.length
  return (
    <div className={`tl-card thread-card${thread.isResolved ? ' is-resolved' : ''}${open ? '' : ' is-collapsed'}`} data-thread-id={thread.id}>
      <button
        type="button"
        className="thread-head"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        aria-controls={open ? bodyId : undefined}
        aria-label={headLabel(thread, lines, chip?.label ?? null, pending)}
      >
        <span className="thread-caret" aria-hidden>
          {open ? '▾' : '▸'}
        </span>
        {reason && reason !== 'file' && lines && <span className="thread-lines">{lines}</span>}
        {reason === 'old-path' && <span className="thread-path">{thread.path}</span>}
        {chip && (
          <span className="chip chip-warn" title={chip.title}>
            {chip.label}
          </span>
        )}
        {!reason && lines?.startsWith('Lines') && <span className="thread-lines">{lines}</span>}
        {!open && first && (
          <span className="thread-preview">
            <span className="comment-author">{first.author?.login ?? 'ghost'}</span> {first.isMinimized ? '(hidden)' : Array.from(first.body.slice(0, PREVIEW_LENGTH * 2)).slice(0, PREVIEW_LENGTH).join('')}
          </span>
        )}
        <span className="spacer" />
        {pending && <span className="chip chip-pending">Pending</span>}
        {thread.isResolved && (
          <span className="chip" title={thread.resolvedBy ? `Resolved by ${thread.resolvedBy}` : undefined}>
            Resolved
          </span>
        )}
        <span className="muted thread-count">
          {count} {count === 1 ? 'comment' : 'comments'}
        </span>
      </button>
      {open && (
        <div className="thread-body" id={bodyId}>
          {reason && reason !== 'file' && <Hunk thread={thread} />}
          {thread.comments.map((c) => (
            <Comment key={c.id} comment={c} />
          ))}
        </div>
      )}
    </div>
  )
})
