import { useEffect, useRef, useState } from 'react'
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
  /** Set when the box's lines refer to an older head than the one shown. */
  staleHead?: boolean
  onSend(mode: CommentMode): void
}

/**
 * A box for a new comment. Without a pending review it offers "Start a review" (the comment
 * waits in a new pending review) and "Add single comment" (posted at once); with one, the
 * comment goes into it. Ctrl+Enter does the first; Esc closes an empty box, or asks first.
 */
export function Composer({ drafts, draftKey, view, pendingReview, locked, label, staleHead, onSend }: Props) {
  const [text, setText] = useState(() => drafts.text(draftKey))
  const [confirming, setConfirming] = useState(false)
  const input = useRef<HTMLTextAreaElement>(null)
  const empty = text.trim() === ''
  const busy = view.sending

  useEffect(() => {
    if (drafts.takeFocus(draftKey)) input.current?.focus()
  })

  const change = (value: string) => {
    setText(value)
    drafts.setText(draftKey, value)
  }
  const send = (mode: CommentMode) => {
    if (!empty && !busy) onSend(mode)
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
    if (e.nativeEvent.isComposing) return
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

  return (
    <div className="tl-card composer" data-draft={draftKey}>
      <textarea
        ref={input}
        className="composer-input"
        aria-label={label}
        placeholder="Leave a comment (Markdown)"
        value={text}
        readOnly={busy}
        onChange={(e) => change(e.target.value)}
        onKeyDown={onKeyDown}
      />
      {staleHead && (
        <div className="composer-note">
          <span className="chip chip-warn">Older version</span> Started on an earlier version of the pull request: check that the lines are still right.
        </div>
      )}
      {locked && <div className="composer-note muted">The conversation is locked: GitHub takes comments from collaborators only.</div>}
      {view.error && (
        <div className="composer-error" role="alert">
          {view.error}
        </div>
      )}
      {confirming ? (
        <div className="composer-actions" onKeyDown={onConfirmKeyDown}>
          <span className="composer-ask">Discard this comment?</span>
          <span className="spacer" />
          <button type="button" className="btn" onClick={keepEditing}>
            Keep editing
          </button>
          <button type="button" className="btn btn-danger" onClick={() => drafts.close(draftKey)} autoFocus>
            Discard
          </button>
        </div>
      ) : (
        <div className="composer-actions">
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
            <button type="button" className="btn btn-primary" onClick={() => send('review')} disabled={empty || busy}>
              Add review comment
            </button>
          ) : (
            <>
              <button type="button" className="btn" onClick={() => send('single')} disabled={empty || busy} title="Post this comment now, on its own">
                Add single comment
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => send('review')}
                disabled={empty || busy}
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
