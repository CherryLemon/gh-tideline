import { createContext, memo, useContext, useEffect, useId, useRef, useState } from 'react'
import type { CommentMode, DiffSide, ReviewComment, ReviewThread } from '../api'
import { boxText, editKey, replyKey, type DraftStore, type DraftsSnapshot } from '../drafts'
import { hunkOf, type LineAnchor, type PanelReason } from '../threads'
import { Composer, focusWhenShown } from './Composer'
import { SafeHtml } from './SafeHtml'

/** What the cards may do with their threads; without it they only show them. */
export interface ThreadActions {
  drafts: DraftStore
  /** The open boxes (replies and edits among them). */
  views: DraftsSnapshot
  /** Whether the viewer has a pending review: replies then go into it. */
  pendingReview: boolean
  locked: boolean
  /** Opens the box for a reply to the thread. */
  reply(thread: ReviewThread): void
  sendReply(key: string, mode: CommentMode): void
  /** Opens the box that edits the comment, with its text. */
  edit(thread: ReviewThread, comment: ReviewComment): void
  saveEdit(key: string): void
  /** Deletes the comment; whether its thread went with it, or null when it failed (reported). */
  remove(threadId: string, commentId: string): Promise<{ threadGone: boolean } | null>
  setResolved(threadId: string, resolved: boolean): void
  /** Focuses the diff: where focus goes when a card goes. */
  focusDiff(): void
}

/** Provided by the page; it reaches the cards in the diff through their portals. */
export const ThreadActionsContext = createContext<ThreadActions | null>(null)

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

/**
 * Whether a request that ends may move focus: it fell to the page (what had it went away), or
 * is still in `within`. Not when the user went on elsewhere meanwhile.
 */
function focusFree(within: Element | null | undefined): boolean {
  const a = document.activeElement
  return !a || a === document.body || !!within?.contains(a)
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

interface CommentProps {
  thread: ReviewThread
  comment: ReviewComment
  actions: ThreadActions | null
  /** After the comment was deleted (the card is gone with its thread); `comment` is its element if still shown. */
  onDeleted(threadGone: boolean, comment: Element | null): void
}

function Comment({ thread, comment, actions, onDeleted }: CommentProps) {
  const [showHidden, setShowHidden] = useState(false)
  /** Asking whether to delete, or deleting. */
  const [deleting, setDeleting] = useState<'ask' | 'busy' | null>(null)
  const root = useRef<HTMLDivElement>(null)
  const deleteButton = useRef<HTMLButtonElement>(null)
  const cancelButton = useRef<HTMLButtonElement>(null)
  /** Set when asking ends without deleting: focus goes back to Delete once it is enabled. */
  const refocus = useRef(false)
  useEffect(() => {
    // Not `autoFocus`: in the diff, focus must not scroll the editor (see `focusWhenShown`).
    if (deleting === 'ask' && cancelButton.current) focusWhenShown(cancelButton.current)
    if (deleting !== null || !refocus.current) return
    refocus.current = false
    if (focusFree(root.current)) deleteButton.current?.focus({ preventScroll: true })
  }, [deleting])
  const askId = useId()
  const whoId = useId()
  const login = comment.author?.login ?? 'ghost'
  const key = editKey(comment.id)
  const editView = actions?.views.get(key)
  const keep = () => {
    refocus.current = true
    setDeleting(null)
  }
  const remove = async () => {
    if (!actions || deleting === 'busy' || editView?.sending) return
    setDeleting('busy')
    const done = await actions.remove(thread.id, comment.id)
    if (done) onDeleted(done.threadGone, root.current)
    else keep()
  }
  return (
    <div ref={root} className="comment" data-comment-id={comment.id}>
      <div className="comment-head">
        <Avatar comment={comment} />
        <span id={`${whoId}-a`} className={`comment-author${comment.author ? '' : ' muted'}`} title={comment.author ? undefined : 'Deleted account'}>
          {login}
        </span>
        <a
          id={`${whoId}-t`}
          className="comment-time"
          href={httpsUrl(comment.url)}
          target="_blank"
          rel="noopener noreferrer"
          title={new Date(comment.createdAt).toLocaleString()}
        >
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
        <span className="spacer" />
        {actions && comment.viewerCanUpdate && (
          <button
            type="button"
            className="link-btn comment-action"
            onClick={() => actions.edit(thread, comment)}
            disabled={deleting !== null}
            aria-describedby={`${whoId}-a ${whoId}-t`}
          >
            Edit
          </button>
        )}
        {actions && comment.viewerCanDelete && (
          <button
            ref={deleteButton}
            type="button"
            className="link-btn comment-action"
            onClick={() => setDeleting('ask')}
            disabled={deleting !== null || editView?.sending}
            aria-describedby={`${whoId}-a ${whoId}-t`}
          >
            Delete
          </button>
        )}
      </div>
      {actions && editView ? (
        <Composer
          drafts={actions.drafts}
          draftKey={key}
          view={editView}
          // Not while it is being deleted: the edit could land after the delete.
          action={{ kind: 'edit', original: boxText(comment.body), deleting: deleting === 'busy', onSave: () => deleting !== 'busy' && actions.saveEdit(key) }}
          label="Edit your comment"
        />
      ) : comment.isMinimized && !showHidden ? (
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
      {deleting && (
        <div
          className="composer-actions comment-confirm"
          role="group"
          aria-labelledby={askId}
          aria-busy={deleting === 'busy'}
          onKeyDown={(e) => {
            if (e.key !== 'Escape' || deleting === 'busy') return
            e.preventDefault()
            keep()
          }}
        >
          <span className="composer-ask" id={askId} aria-live="polite">
            {deleting === 'busy' ? 'Deleting…' : 'Delete this comment?'}
          </span>
          <span className="spacer" />
          <button ref={cancelButton} type="button" className="btn" onClick={keep} disabled={deleting === 'busy'}>
            Cancel
          </button>
          {/* Not disabled while deleting: focus stays on it (a disabled button drops it). */}
          <button
            type="button"
            className="btn btn-danger"
            onClick={() => void remove()}
            aria-disabled={deleting === 'busy' || editView?.sending}
            disabled={deleting !== 'busy' && editView?.sending}
          >
            Delete
          </button>
        </div>
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
function headLabel(thread: ReviewThread, lines: string | null, chip: string | null, pending: boolean, draft: boolean): string {
  const first = thread.comments[0]
  const count = thread.comments.length
  const where = thread.subjectType === 'FILE' ? 'Comment on the file' : `Thread${lines ? ` on ${lines.toLowerCase()}` : ''}`
  return [
    `${where}${first ? ` by ${first.author?.login ?? 'ghost'}` : ''}`,
    `${count} ${count === 1 ? 'comment' : 'comments'}`,
    chip?.toLowerCase(),
    pending && 'pending',
    thread.isResolved && 'resolved',
    draft && 'with your draft',
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
  const actions = useContext(ThreadActionsContext)
  const bodyId = useId()
  const replyBox = replyKey(thread.id)
  const replyView = actions?.views.get(replyBox)
  const drafting = replyView !== undefined || thread.comments.some((c) => actions?.views.has(editKey(c.id)))
  // A box left open (say before a reload) shows.
  const [open, setOpen] = useState((!thread.isCollapsed && reason !== 'outdated') || drafting)
  // Follow GitHub when it collapses or expands the thread (resolving does).
  const [collapsedWas, setCollapsedWas] = useState(thread.isCollapsed)
  if (collapsedWas !== thread.isCollapsed) {
    setCollapsedWas(thread.isCollapsed)
    setOpen(!thread.isCollapsed)
  }
  const card = useRef<HTMLDivElement>(null)
  const head = useRef<HTMLButtonElement>(null)
  // Focus left on a button that went away (a deleted comment, a card folded by resolving).
  const focusHead = () => head.current?.focus({ preventScroll: true })
  // Focus goes on only from what went: the card with its thread, else the comment (not from
  // the reply box the user went on to, say).
  const onDeleted = (threadGone: boolean, comment: Element | null) => {
    if (threadGone) {
      if (focusFree(card.current)) actions?.focusDiff()
    } else if (focusFree(comment)) focusHead()
  }
  const first = thread.comments[0]
  const lines = threadLines(thread)
  const chip = reason ? REASON_CHIPS[reason] : null
  const pending = thread.comments.some((c) => c.state === 'PENDING')
  const count = thread.comments.length
  const canToggle = thread.isResolved ? thread.viewerCanUnresolve : thread.viewerCanResolve
  return (
    <div ref={card} className={`tl-card thread-card${thread.isResolved ? ' is-resolved' : ''}${open ? '' : ' is-collapsed'}`} data-thread-id={thread.id}>
      <button
        ref={head}
        type="button"
        className="thread-head"
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        aria-controls={open ? bodyId : undefined}
        aria-label={headLabel(thread, lines, chip?.label ?? null, pending, !open && drafting)}
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
        {!open && drafting && (
          <span className="chip chip-warn" title="You have a comment box open in this thread">
            Draft
          </span>
        )}
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
            <Comment key={c.id} thread={thread} comment={c} actions={actions} onDeleted={onDeleted} />
          ))}
          {actions && replyView && (
            <Composer
              drafts={actions.drafts}
              draftKey={replyBox}
              view={replyView}
              action={{ kind: 'comment', pendingReview: actions.pendingReview, onSend: (mode) => actions.sendReply(replyBox, mode) }}
              locked={actions.locked}
              label={`Reply to ${first?.author?.login ?? 'ghost'}'s thread${lines ? ` on ${lines.toLowerCase()}` : ''}`}
              placeholder="Reply (Markdown)"
            />
          )}
          {actions && (thread.viewerCanReply || canToggle) && (
            <div className="thread-actions">
              {thread.viewerCanReply && (
                // Stays while the box is open: the box gives focus back to it when it closes.
                <button type="button" className="btn" onClick={() => actions.reply(thread)}>
                  Reply
                </button>
              )}
              <span className="spacer" />
              {canToggle && (
                <button
                  type="button"
                  className="btn"
                  onClick={() => {
                    actions.setResolved(thread.id, !thread.isResolved)
                    // Resolving folds the card.
                    if (!thread.isResolved) focusHead()
                  }}
                >
                  {thread.isResolved ? 'Unresolve conversation' : 'Resolve conversation'}
                </button>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  )
})
