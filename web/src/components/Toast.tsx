import { useEffect } from 'react'

export interface ToastMessage {
  /** Distinguishes consecutive toasts with the same text, so each gets its full time. */
  id: number
  text: string
  action?: { label: string; run(): void }
}

const TOAST_MS = 3000

/**
 * Notice at the bottom of the page. Plain ones go away after 3 s; one with an action stays
 * until it is used or closed, so the button cannot vanish under the pointer. `onDismiss`
 * must be stable.
 */
export function Toast({ toast, onDismiss }: { toast: ToastMessage; onDismiss(): void }) {
  const { action } = toast
  useEffect(() => {
    if (action) return
    const timer = setTimeout(onDismiss, TOAST_MS)
    return () => clearTimeout(timer)
  }, [toast.id, action, onDismiss])

  return (
    <div className="toast" role="status">
      <span className="toast-text">{toast.text}</span>
      {action && (
        <button
          className="btn"
          onClick={() => {
            onDismiss()
            action.run()
          }}
        >
          {action.label}
        </button>
      )}
      {action && (
        <button className="toast-close" onClick={onDismiss} title="Close" aria-label="Close">
          ×
        </button>
      )}
    </div>
  )
}
