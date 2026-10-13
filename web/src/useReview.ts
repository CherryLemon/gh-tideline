// The review threads of the pull request a page shows, kept in one `ReviewStore` for the life
// of the page: reads that come with each page load or refresh go through `rebase`.
import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react'
import type { ReviewThreads } from './api'
import { errorMessage } from './api'
import { ReviewStore, type ReviewSnapshot, type SendResolved } from './review'

export interface ReviewRead {
  promise: Promise<ReviewThreads>
  /** `performance.now()` when it was sent. */
  requestedAt: number
}

export interface Review {
  store: ReviewStore | null
  /** null until a read succeeds. */
  snapshot: ReviewSnapshot | null
  /** Whether a read is in flight, or the page's latest one not taken yet. */
  reading: boolean
  /** Why the latest read failed (a new object per failure); null once one succeeds. */
  error: { message: string } | null
  /** Reads again with `read`. */
  retry(): void
}

const noSubscribe = () => () => {}
const noSnapshot = () => null

/**
 * `latest`: the read that came with the page's latest load. `read` starts another (retry).
 * `sendResolved` and `onResolveError` may change identity; the latest ones are used.
 */
export function useReview(
  latest: ReviewRead,
  read: () => Promise<ReviewThreads>,
  sendResolved: SendResolved,
  onResolveError: (threadId: string, error: unknown) => void,
): Review {
  const [store, setStore] = useState<ReviewStore | null>(null)
  const [inflight, setInflight] = useState(0)
  const [error, setError] = useState<{ message: string } | null>(null)
  const [taken, setTaken] = useState<ReviewRead | null>(null)
  const storeRef = useRef<ReviewStore | null>(null)
  const live = useRef(true)
  /** When the newest read that settled was sent: older ones neither set nor clear the error. */
  const settledAt = useRef(-Infinity)
  const callbacks = useRef({ read, sendResolved, onResolveError })
  callbacks.current = { read, sendResolved, onResolveError }

  useEffect(() => {
    live.current = true
    return () => {
      live.current = false
      storeRef.current?.dispose()
      storeRef.current = null
    }
  }, [])

  const take = useCallback(({ promise, requestedAt }: ReviewRead) => {
    setInflight((n) => n + 1)
    const settle = (err: { message: string } | null) => {
      if (requestedAt < settledAt.current) return false
      settledAt.current = requestedAt
      setError(err)
      return true
    }
    promise
      .then(
        (threads) => {
          if (!live.current) return
          settle(null)
          let s = storeRef.current
          if (!s) {
            s = new ReviewStore(
              threads,
              (id, resolved) => callbacks.current.sendResolved(id, resolved),
              (id, e) => callbacks.current.onResolveError(id, e),
            )
            storeRef.current = s
            setStore(s)
          }
          s.rebase(threads, requestedAt)
        },
        (e: unknown) => {
          if (live.current) settle({ message: errorMessage(e) })
        },
      )
      .finally(() => {
        if (live.current) setInflight((n) => n - 1)
      })
  }, [])

  useEffect(() => {
    take(latest)
    setTaken(latest)
  }, [latest, take])

  const retry = useCallback(() => take({ promise: callbacks.current.read(), requestedAt: performance.now() }), [take])

  const snapshot = useSyncExternalStore(store?.subscribe ?? noSubscribe, store?.getSnapshot ?? noSnapshot)
  return { store, snapshot, reading: inflight > 0 || taken !== latest, error, retry }
}
