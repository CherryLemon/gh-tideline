import { useEffect } from 'react'

export interface ToastMessage {
  /** Distinguishes consecutive toasts with the same text, so each gets its full time. */
  id: number
  text: string
  action?: { label: string; run(): void }
}

const TOAST_MS = 3000

/** Transient notice at the bottom of the page; `onDismiss` must be stable. */
export function Toast({ toast, onDismiss }: { toast: ToastMessage; onDismiss(): void }) {
  useEffect(() => {
    const timer = setTimeout(onDismiss, TOAST_MS)
    return () => clearTimeout(timer)
  }, [toast.id, onDismiss])

  const { action } = toast
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
    </div>
  )
}
