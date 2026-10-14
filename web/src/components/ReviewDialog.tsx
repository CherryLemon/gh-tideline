import { useEffect, useId, useRef, useState } from 'react'
import type { PendingReview, ReviewEvent } from '../api'
import { draftKey, type DraftStore, type DraftView } from '../drafts'
import { focusWhenShown } from '../focus'
import { MarkdownField } from './MarkdownField'

/** The summary's box in the drafts. */
export const REVIEW_KEY = draftKey({ kind: 'review' })

interface Props {
  drafts: DraftStore
  /** The summary's draft. */
  view: DraftView
  pendingReview: PendingReview | null
  /** Comments in the pending review. */
  pendingCount: number
  /** The head the page shows. */
  head: string
  /** The viewer wrote the pull request: GitHub refuses their approval or request for changes. */
  own: boolean
  /** The conversation is locked (GitHub takes reviews from collaborators only). */
  locked: boolean
  onSubmit(event: ReviewEvent): void
  /** Discards the pending review; resolves with why it failed, or null once done. */
  onDiscard(): Promise<string | null>
  onClose(): void
}

const EVENTS: { event: ReviewEvent; label: string; hint: string }[] = [
  { event: 'COMMENT', label: 'Comment', hint: 'General feedback, without approving or asking for changes' },
  { event: 'APPROVE', label: 'Approve', hint: 'The changes are good to merge' },
  { event: 'REQUEST_CHANGES', label: 'Request changes', hint: 'The changes need more work before they are merged' },
]

const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? '' : 's'}`

/**
 * Finishes the review: a summary (kept with the drafts), Comment, Approve or Request changes,
 * and the pending review's comments go out with it. Also discards the pending review, after
 * asking. Modal: the page behind is inert while it is open. Ctrl+Enter submits; Esc closes it
 * (the summary is kept), or takes back a discard about to happen.
 */
export function ReviewDialog({ drafts, view, pendingReview, pendingCount, head, own, locked, onSubmit, onDiscard, onClose }: Props) {
  const [text, setText] = useState(() => drafts.text(REVIEW_KEY))
  const [event, setEvent] = useState<ReviewEvent>('COMMENT')
  const [preview, setPreview] = useState(false)
  /** The pending review a discard was asked for. */
  const [confirming, setConfirming] = useState<string | null>(null)
  const [discarding, setDiscarding] = useState(false)
  const [discardError, setDiscardError] = useState<string | null>(null)
  const input = useRef<HTMLTextAreaElement>(null)
  const keepButton = useRef<HTMLButtonElement>(null)
  const live = useRef(true)
  const id = useId()
  // Read while rendering, before the box takes focus: focus goes back there on closing.
  const [opener] = useState(() => document.activeElement)
  const busy = view.sending || discarding
  // A read that drops (or replaces) the pending review ends a discard about to happen.
  const asking = confirming !== null && confirming === pendingReview?.id
  const comments = pendingReview ? pendingCount : 0
  const chosen = own && event !== 'COMMENT' ? 'COMMENT' : event
  // GitHub refuses a review with neither comments nor a summary, unless it approves.
  const needsSummary = comments === 0 && text.trim() === '' && chosen !== 'APPROVE'
  const olderCommit = pendingReview?.commit != null && pendingReview.commit !== head ? pendingReview.commit : null
  const notes = [needsSummary && `${id}-needs`, olderCommit && `${id}-older`, locked && `${id}-locked`].filter(Boolean).join(' ')

  useEffect(() => {
    live.current = true
    drafts.takeFocus(REVIEW_KEY)
    if (input.current) focusWhenShown(input.current)
    return () => {
      live.current = false
      const lost = document.activeElement === null || document.activeElement === document.body
      if (lost && opener instanceof HTMLElement && opener !== document.body && opener.isConnected) opener.focus({ preventScroll: true })
    }
  }, [drafts, opener])
  useEffect(() => {
    if (asking && keepButton.current) focusWhenShown(keepButton.current)
  }, [asking])
  // A button disabled while busy may have taken the focus with it: back to the summary.
  useEffect(() => {
    const lost = document.activeElement === null || document.activeElement === document.body
    if (!busy && lost && input.current) {
      setPreview(false)
      focusWhenShown(input.current)
    }
  }, [busy])

  const change = (value: string) => {
    setText(value)
    drafts.setText(REVIEW_KEY, value)
  }
  const submit = () => {
    if (busy || asking || needsSummary) return
    setDiscardError(null)
    onSubmit(chosen)
  }
  const keep = () => {
    setConfirming(null)
    setPreview(false)
    if (input.current) focusWhenShown(input.current)
  }
  const discard = () => {
    setDiscarding(true)
    setDiscardError(null)
    void onDiscard().then((error) => {
      if (!live.current) return
      setDiscarding(false)
      setDiscardError(error)
      if (error !== null) keep()
    })
  }

  const onKeyDown = (e: React.KeyboardEvent) => {
    // Also Safari's Esc ending a composition (`isComposing` already false there).
    if (e.nativeEvent.isComposing || e.keyCode === 229) return
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey) {
      e.preventDefault()
      submit()
    } else if (e.key === 'Escape') {
      e.preventDefault()
      if (busy) return
      if (asking) keep()
      else onClose()
    }
  }

  // The newest: a submit clears the discard's.
  const error = discardError ?? view.error
  return (
    <div className="dialog-backdrop" onMouseDown={() => !busy && onClose()}>
      <div
        className="dialog review-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={`${id}-title`}
        aria-busy={busy}
        // Takes the focus from a click on its text, so its keys still work.
        tabIndex={-1}
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={onKeyDown}
      >
        <div className="dialog-title" id={`${id}-title`}>
          {pendingReview ? 'Finish your review' : 'Review changes'}
        </div>
        <div className="dialog-body">
          {olderCommit && (
            <div className="composer-note" id={`${id}-older`}>
              <span className="chip chip-warn">Older version</span> Your pending review was started on an earlier version of the pull request (
              <code>{olderCommit.slice(0, 7)}</code>): its comments stay on the lines of that version.
            </div>
          )}
          <MarkdownField
            input={input}
            value={text}
            onChange={change}
            preview={preview}
            onPreview={setPreview}
            label="Summary of your review"
            describedBy={notes || undefined}
            placeholder="Leave a summary (Markdown)"
            readOnly={busy}
          />
          <fieldset className="review-events" disabled={busy} aria-label="Your verdict">
            {EVENTS.map((o) => {
              const refused = own && o.event !== 'COMMENT'
              return (
                <label key={o.event} className={`review-event${refused ? ' is-disabled' : ''}`}>
                  <input type="radio" name={`${id}-event`} value={o.event} checked={chosen === o.event} disabled={refused} onChange={() => setEvent(o.event)} />
                  <span className="review-event-label">{o.label}</span>
                  <span className="muted">{refused ? "GitHub doesn't take this on your own pull request" : o.hint}</span>
                </label>
              )
            })}
          </fieldset>
          {locked && (
            <div className="composer-note muted" id={`${id}-locked`}>
              The conversation is locked: GitHub takes reviews from collaborators only.
            </div>
          )}
          {error && (
            <div className="composer-error" role="alert">
              {error}
            </div>
          )}
        </div>
        {asking ? (
          // Keyed: a new row, not the Discard button reused for Keep.
          <div key="ask" className="composer-actions dialog-actions" role="group" aria-labelledby={`${id}-ask`}>
            <span className="composer-ask" id={`${id}-ask`}>
              {comments > 0 ? `Discard your pending review and its ${plural(comments, 'comment')}?` : 'Discard your pending review?'}
            </span>
            <span className="spacer" />
            <button ref={keepButton} type="button" className="btn" onClick={keep} disabled={discarding}>
              Keep it
            </button>
            <button type="button" className="btn btn-danger" onClick={discard} disabled={discarding}>
              {discarding ? 'Discarding…' : 'Discard'}
            </button>
          </div>
        ) : (
          <div key="send" className="composer-actions dialog-actions">
            {pendingReview && (
              <button
                type="button"
                className="btn btn-danger"
                onClick={() => setConfirming(pendingReview.id)}
                disabled={busy}
                title="Delete your pending review and its comments"
              >
                Discard review
              </button>
            )}
            <span className="muted composer-hint" id={`${id}-needs`} aria-live="polite">
              {view.sending
                ? 'Submitting…'
                : needsSummary
                  ? 'Write a summary: a review with no comments needs one'
                  : pendingReview
                    ? `${plural(comments, 'pending comment')} go${comments === 1 ? 'es' : ''} out with it`
                    : 'Just the summary: you have no pending comments'}
            </span>
            <span className="spacer" />
            <button type="button" className="btn" onClick={onClose} disabled={busy}>
              Cancel
            </button>
            <button type="button" className="btn btn-primary" onClick={submit} disabled={busy || needsSummary} title="Ctrl+Enter">
              Submit review
            </button>
          </div>
        )}
      </div>
    </div>
  )
}
