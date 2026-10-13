import { useEffect, useId, useRef, useState } from 'react'
import type { CommentMode } from '../api'
import type { DraftStore, DraftView } from '../drafts'

interface Props {
  drafts: DraftStore
  draftKey: string
  view: DraftView
  /** Whether the viewer has a pending review: new comments then go into it. */
  pendingReview: boolean
  /** The conversation is locked (GitHub takes comments from collaborators only). */
  locked: boolean
  /** Says what the box is for, to assistive tech. */
  label: string
  /** Shown above the text, for a box away from its lines. */
  heading?: React.ReactNode
  /**
   * Set when the box's lines refer to an older head than the one shown: sending is refused
   * until the user keeps the lines for the shown head.
   */
  onKeepLines?(): void
  onSend(mode: CommentMode): void
}

/** Frames to wait for a new box to show (a box in the diff waits for its zone) before focusing. */
const FOCUS_FRAMES = 30

/**
 * Focuses `el` once it can take focus. Inside the diff editor the page must not scroll: the
 * editor scrolls itself (`ZoneHost`), and a scrolled editor container misplaces everything.
 */
function focusWhenShown(el: HTMLElement, frames = FOCUS_FRAMES): void {
  if (!el.isConnected) return
  el.focus({ preventScroll: !!el.closest('.tl-zone') })
  if (document.activeElement !== el && frames > 0) requestAnimationFrame(() => focusWhenShown(el, frames - 1))
}

/**
 * A box for a new comment. Without a pending review it offers "Start a review" (the comment
 * waits in a new pending review) and "Add single comment" (posted at once); with one, the
 * comment goes into it. Ctrl+Enter does the first; Esc closes an empty box, or asks first.
 */
export function Composer({ drafts, draftKey, view, pendingReview, locked, label, heading, onKeepLines, onSend }: Props) {
  const [text, setText] = useState(() => drafts.text(draftKey))
  const [confirming, setConfirming] = useState(false)
  const input = useRef<HTMLTextAreaElement>(null)
  /** Where focus was when the box took it: it goes back there when the box closes. */
  const returnTo = useRef<HTMLElement | null>(null)
  const id = useId()
  const empty = text.trim() === ''
  const busy = view.sending
  const stale = onKeepLines !== undefined
  const blocked = empty || busy || stale

  useEffect(() => {
    if (!drafts.takeFocus(draftKey) || !input.current) return
    const from = document.activeElement
    if (from instanceof HTMLElement && from !== document.body) returnTo.current = from
    focusWhenShown(input.current)
  })
  useEffect(
    () => () => {
      // Closed with focus inside: it fell to the page.
      const back = returnTo.current
      if (back?.isConnected && (document.activeElement === null || document.activeElement === document.body)) back.focus({ preventScroll: true })
    },
    [],
  )

  const change = (value: string) => {
    setText(value)
    drafts.setText(draftKey, value)
  }
  const send = (mode: CommentMode) => {
    if (!blocked && !confirming) onSend(mode)
  }
  const cancel = () => {
    if (empty) drafts.close(draftKey)
    else setConfirming(true)
  }
  const keepEditing = () => {
    setConfirming(false)
    input.current?.focus()
  }

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Also Safari's Esc ending a composition (`isComposing` already false there).
    if (e.nativeEvent.isComposing || e.keyCode === 229) return
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey) {
      e.preventDefault()
      send('review')
    } else if (e.key === 'Escape' && !busy) {
      e.preventDefault()
      cancel()
    }
  }

  const onConfirmKeyDown = (e: React.KeyboardEvent) => {
    if (e.key !== 'Escape') return
    e.preventDefault()
    keepEditing()
  }

  const notes = [stale && `${id}-stale`, locked && `${id}-locked`, view.error && `${id}-error`].filter(Boolean).join(' ')

  return (
    <div className="tl-card composer" data-draft={draftKey} aria-busy={busy}>
      {heading && <div className="composer-heading">{heading}</div>}
      <textarea
        ref={input}
        className="composer-input"
        aria-label={label}
        aria-describedby={notes || undefined}
        placeholder="Leave a comment (Markdown)"
        value={text}
        readOnly={busy}
        onChange={(e) => change(e.target.value)}
        onKeyDown={onKeyDown}
      />
      {stale && (
        <div className="composer-note" id={`${id}-stale`}>
          <span className="chip chip-warn">Older version</span> Started on an earlier version of the pull request: check that the lines are
          still right.{' '}
          <button type="button" className="link-btn" onClick={onKeepLines} disabled={busy}>
            Keep these lines
          </button>
        </div>
      )}
      {locked && (
        <div className="composer-note muted" id={`${id}-locked`}>
          The conversation is locked: GitHub takes comments from collaborators only.
        </div>
      )}
      {view.error && (
        <div className="composer-error" role="alert" id={`${id}-error`}>
          {view.error}
        </div>
      )}
      {confirming ? (
        // Keyed: a new row, so `autoFocus` applies (React would reuse Cancel's button).
        <div key="ask" className="composer-actions" role="group" aria-labelledby={`${id}-ask`} onKeyDown={onConfirmKeyDown}>
          <span className="composer-ask" id={`${id}-ask`}>
            Discard this comment?
          </span>
          <span className="spacer" />
          <button type="button" className="btn" onClick={keepEditing} autoFocus>
            Keep editing
          </button>
          <button type="button" className="btn btn-danger" onClick={() => drafts.close(draftKey)} disabled={busy}>
            Discard
          </button>
        </div>
      ) : (
        <div key="send" className="composer-actions">
          <span className="muted composer-hint" aria-live="polite">
            {busy ? (
              'Sending…'
            ) : (
              <>
                <kbd>Ctrl+Enter</kbd> {pendingReview ? 'add' : 'start a review'}
              </>
            )}
          </span>
          <span className="spacer" />
          <button type="button" className="btn" onClick={cancel} disabled={busy}>
            Cancel
          </button>
          {pendingReview ? (
            <button type="button" className="btn btn-primary" onClick={() => send('review')} disabled={blocked}>
              Add review comment
            </button>
          ) : (
            <>
              <button type="button" className="btn" onClick={() => send('single')} disabled={blocked} title="Post this comment now, on its own">
                Add single comment
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => send('review')}
                disabled={blocked}
                title="Keep this comment pending until you submit your review"
              >
                Start a review
              </button>
            </>
          )}
        </div>
      )}
    </div>
  )
}
