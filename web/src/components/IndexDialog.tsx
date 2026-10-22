import { useEffect, useId, useRef, useState } from 'react'
import type { IndexStatus, NavSide } from '../api'
import { focusWhenShown } from '../focus'

/** How far the build of an index has got, or why it failed. */
export interface IndexProgress {
  /** Which commit is indexed: the head, or the merge base for a look-up from the old side of a diff. */
  side: NavSide
  phase: IndexStatus['phase']
  done: number
  total: number
  /** Why the build failed, once it has. */
  failure: string | null
}

interface Props extends IndexProgress {
  /** Stops the build. */
  onCancel(): void
  onRetry(): void
  onClose(): void
}

const PHASES: Record<NonNullable<IndexStatus['phase']>, string> = {
  listing: 'Listing files',
  fetching: 'Downloading files',
  parsing: 'Parsing',
  saving: 'Saving',
}

/**
 * Shown when a look-up has to wait for the index of the repository to be built, which is done once for a commit and
 * takes a while for a big repository. Modal: the page behind is inert, and a click outside does nothing, as stopping
 * the build is Cancel's job (Esc too). A build that failed is offered a Retry.
 */
export function IndexDialog({ side, phase, done, total, failure, onCancel, onRetry, onClose }: Props) {
  const id = useId()
  const primary = useRef<HTMLButtonElement>(null)
  // Read while rendering, before the button takes focus: focus goes back there on closing.
  const [opener] = useState(() => document.activeElement)
  const failed = failure !== null

  useEffect(
    () => () => {
      const lost = document.activeElement === null || document.activeElement === document.body
      if (lost && opener instanceof HTMLElement && opener !== document.body && opener.isConnected) opener.focus({ preventScroll: true })
    },
    [opener],
  )
  // The button that Esc stands for, or the one that tries again.
  useEffect(() => {
    if (primary.current) focusWhenShown(primary.current)
  }, [failed])

  const onKeyDown = (e: React.KeyboardEvent) => {
    // Also Safari's Esc ending a composition (`isComposing` already false there).
    if (e.nativeEvent.isComposing || e.keyCode === 229) return
    if (e.key === 'Escape') {
      e.preventDefault()
      if (failed) onClose()
      else onCancel()
    }
  }

  const known = total > 0
  const counts = known ? ` · ${Math.min(done, total).toLocaleString()} / ${total.toLocaleString()}` : ''
  return (
    // The press must not take the focus from the dialog.
    <div className="dialog-backdrop" onMouseDown={(e) => e.preventDefault()}>
      <div
        className="dialog index-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={`${id}-title`}
        aria-describedby={`${id}-what`}
        // Takes the focus from a click on its text, so its keys still work.
        tabIndex={-1}
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={onKeyDown}
      >
        <div className="dialog-title" id={`${id}-title`}>
          Indexing the repository
        </div>
        <div className="dialog-body">
          <div id={`${id}-what`}>
            Looking up a name needs an index of the repository at the {side === 'base' ? 'merge base' : 'head'} of the pull request. It is built once for
            each commit.
          </div>
          {failed ? (
            <div className="composer-error" role="alert">
              {failure}
            </div>
          ) : (
            <>
              <div className="index-phase muted">
                {(phase && PHASES[phase]) ?? 'Starting'}
                {counts}
              </div>
              <div
                className="progress index-progress"
                role="progressbar"
                aria-label="Indexing"
                aria-valuemin={known ? 0 : undefined}
                aria-valuemax={known ? total : undefined}
                aria-valuenow={known ? Math.min(done, total) : undefined}
              >
                <div className={`progress-bar${known ? '' : ' is-indeterminate'}`} style={known ? { width: `${(Math.min(done, total) / total) * 100}%` } : undefined} />
              </div>
            </>
          )}
        </div>
        <div className="composer-actions dialog-actions">
          <span className="spacer" />
          {failed ? (
            <>
              <button type="button" className="btn" onClick={onClose}>
                Close
              </button>
              <button key="retry" ref={primary} type="button" className="btn btn-primary" onClick={onRetry}>
                Retry
              </button>
            </>
          ) : (
            <button key="cancel" ref={primary} type="button" className="btn" onClick={onCancel}>
              Cancel
            </button>
          )}
        </div>
      </div>
    </div>
  )
}
