// Review threads as the page shows them: the last read from GitHub, plus what our own writes
// returned since, plus optimistic resolve/unresolve. Read with `useSyncExternalStore`.
import type { DiffSide, PendingReview, ReviewComment, ReviewThread, ReviewThreads, SubmittedReview } from './api'

export type SendResolved = (threadId: string, resolved: boolean) => Promise<ReviewThread>

export interface ReviewSnapshot {
  /** The head the threads' line numbers refer to. */
  head: string
  locked: boolean
  pendingReview: PendingReview | null
  latestReview: SubmittedReview | null
  /** Resolve intents applied. */
  threads: readonly ReviewThread[]
  /** Comments in the viewer's pending review. */
  pendingCount: number
  /** Threads with a resolve request queued or in flight. */
  busy: ReadonlySet<string>
}

/** The pending and the latest review are kept apart from thread ids in `settled`. */
const PENDING_KEY = ' pending'
const LATEST_KEY = ' latest'

let lastStamp = -Infinity

/**
 * When a read was sent or a write settled, for `ReviewStore.rebase`: `performance.now()`, but
 * strictly increasing. Browsers coarsen it (to 100 µs, or 1 ms), and a read sent right after a
 * write settled must still count as the newer one.
 */
export function stamp(): number {
  lastStamp = Math.max(performance.now(), lastStamp + 1e-3)
  return lastStamp
}

/** A comment of the viewer's pending review `reviewId`. */
const inReview = (c: ReviewComment, reviewId: string) => c.state === 'PENDING' && c.reviewId === reviewId

/**
 * One instance lives as long as the page; reads of every head go through `rebase`. Writes
 * report what GitHub returned through the `apply*` methods; `rebase` keeps anything a write
 * changed after that read was requested (GitHub's answer to the write is the newer one, at
 * whatever head). Resolve is optimistic with at most one request in flight per thread (the
 * latest intent wins; a failure rolls back), like `ViewedSync`: an overlay on the threads.
 */
export class ReviewStore {
  private data: ReviewThreads
  private readonly intent = new Map<string, boolean>()
  private readonly inflight = new Set<string>()
  /** When a write last settled for a thread id (or the pending review), `stamp()`. */
  private readonly settled = new Map<string, number>()
  private rebasedAt = -Infinity
  private readonly listeners = new Set<() => void>()
  private snap: ReviewSnapshot | null = null
  private disposed = false

  constructor(
    initial: ReviewThreads,
    private readonly sendResolved: SendResolved,
    private readonly onError: (threadId: string, error: unknown) => void,
  ) {
    this.data = initial
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  getSnapshot = (): ReviewSnapshot => {
    if (!this.snap) {
      const threads = this.data.threads.map((t) => {
        const want = this.intent.get(t.id)
        if (want === undefined || want === t.isResolved) return t
        // The confirmed permissions are for the other state; whoever may do one may undo it.
        const can = t.viewerCanResolve || t.viewerCanUnresolve
        return { ...t, isResolved: want, isCollapsed: want, viewerCanResolve: can, viewerCanUnresolve: can }
      })
      let pendingCount = 0
      for (const t of threads) for (const c of t.comments) if (c.state === 'PENDING') pendingCount++
      this.snap = {
        head: this.data.head,
        locked: this.data.locked,
        pendingReview: this.data.pendingReview,
        latestReview: this.data.latestReview,
        threads,
        pendingCount,
        busy: new Set([...this.inflight, ...this.intent.keys()]),
      }
    }
    return this.snap
  }

  /**
   * Takes a full read requested at `requestedAt` (`stamp()`). Threads (and the
   * pending review) that a write changed after the read was sent keep their local version:
   * the read may predate the write. Resolve intents stay on top of the read's version (its
   * lines may be for a new head). A read requested before one already taken is ignored.
   */
  rebase(next: ReviewThreads, requestedAt: number): void {
    if (this.disposed || requestedAt < this.rebasedAt) return
    this.rebasedAt = requestedAt
    const newer = (id: string) => (this.settled.get(id) ?? -Infinity) >= requestedAt
    const local = new Map(this.data.threads.map((t) => [t.id, t]))
    const threads: ReviewThread[] = []
    const seen = new Set<string>()
    for (const t of next.threads) {
      seen.add(t.id)
      if (!newer(t.id)) threads.push(t)
      else if (local.has(t.id)) threads.push(local.get(t.id)!)
      // Deleted by a write after the read was sent.
    }
    // Created by a write after the read was sent.
    for (const t of this.data.threads) if (!seen.has(t.id) && newer(t.id)) threads.push(t)
    for (const id of this.intent.keys()) if (!threads.some((t) => t.id === id)) this.intent.delete(id)
    this.data = {
      head: next.head,
      locked: next.locked,
      pendingReview: newer(PENDING_KEY) ? this.data.pendingReview : next.pendingReview,
      latestReview: newer(LATEST_KEY) ? this.data.latestReview : next.latestReview,
      threads,
    }
    this.changed()
  }

  /** A thread as a write returned it (created, or changed); null removes it (deleted). */
  applyThread(id: string, thread: ReviewThread | null): void {
    if (this.disposed) return
    this.settled.set(id, stamp())
    const threads = this.data.threads.filter((t) => t.id !== id)
    const at = this.data.threads.findIndex((t) => t.id === id)
    if (thread) threads.splice(at < 0 ? threads.length : at, 0, thread)
    this.data = { ...this.data, threads }
    this.changed()
  }

  /**
   * A comment deleted: out of its thread, or with it (`threadGone`, it was the last one). The
   * stored thread, not the snapshot: a resolve still on its way stays one. Whether it was there.
   */
  dropComment(threadId: string, commentId: string, threadGone: boolean): boolean {
    const t = this.data.threads.find((t) => t.id === threadId)
    if (this.disposed || !t?.comments.some((c) => c.id === commentId)) return false
    this.applyThread(threadId, threadGone ? null : { ...t, comments: t.comments.filter((c) => c.id !== commentId) })
    return true
  }

  /**
   * A comment a write returned: a reply (added) or an edit (replaced; an edit of a comment
   * deleted meanwhile doesn't bring it back). Whether the store has its thread (and, for an
   * edit, the comment).
   */
  applyComment(threadId: string, comment: ReviewComment, edit = false): boolean {
    const t = this.data.threads.find((t) => t.id === threadId)
    if (this.disposed || !t) return false
    const at = t.comments.findIndex((c) => c.id === comment.id)
    if (at < 0 && edit) return false
    const comments = at < 0 ? [...t.comments, comment] : t.comments.map((c, i) => (i === at ? comment : c))
    this.applyThread(threadId, { ...t, comments })
    return true
  }

  /** The viewer's pending review after a write (null: none any more). */
  applyPendingReview(review: PendingReview | null): void {
    if (this.disposed) return
    this.settled.set(PENDING_KEY, stamp())
    this.data = { ...this.data, pendingReview: review }
    this.changed()
  }

  /**
   * A review submitted: the comments of the viewer's pending review `reviewId` (null: a review
   * of just a summary) are no longer pending (the next read brings the rest), and `review` is
   * the latest one (null: GitHub didn't send it back).
   */
  applySubmitted(review: SubmittedReview | null, reviewId: string | null): void {
    if (this.disposed) return
    const now = stamp()
    let { threads, pendingReview } = this.data
    if (reviewId !== null) {
      threads = threads.map((t) => {
        if (!t.comments.some((c) => inReview(c, reviewId))) return t
        this.settled.set(t.id, now)
        return { ...t, comments: t.comments.map((c): ReviewComment => (inReview(c, reviewId) ? { ...c, state: 'SUBMITTED' } : c)) }
      })
      // Not another pending review a read brought meanwhile.
      if (pendingReview?.id === reviewId) pendingReview = null
      this.settled.set(PENDING_KEY, now)
    }
    if (review) this.settled.set(LATEST_KEY, now)
    this.data = { ...this.data, threads, pendingReview, latestReview: review ?? this.data.latestReview }
    this.changed()
  }

  /** The viewer's pending review `reviewId` discarded: its comments go, and the threads left without any. */
  applyDiscard(reviewId: string): void {
    if (this.disposed) return
    const now = stamp()
    const { threadIds } = discarded(this.data.threads, reviewId)
    const threads: ReviewThread[] = []
    for (const t of this.data.threads) {
      if (!t.comments.some((c) => inReview(c, reviewId))) {
        threads.push(t)
        continue
      }
      this.settled.set(t.id, now)
      if (!threadIds.includes(t.id)) threads.push({ ...t, comments: t.comments.filter((c) => !inReview(c, reviewId)) })
    }
    this.settled.set(PENDING_KEY, now)
    const pendingReview = this.data.pendingReview?.id === reviewId ? null : this.data.pendingReview
    this.data = { ...this.data, threads, pendingReview }
    this.changed()
  }

  setResolved(threadId: string, resolved: boolean): void {
    if (this.disposed || !this.data.threads.some((t) => t.id === threadId)) return
    this.intent.set(threadId, resolved)
    this.changed()
    if (!this.inflight.has(threadId)) this.pump(threadId)
  }

  /** Stops notifying and sending; a request in flight completes, nothing follows it. */
  dispose(): void {
    this.disposed = true
    this.listeners.clear()
  }

  private changed(): void {
    this.snap = null
    for (const l of [...this.listeners]) l()
  }

  private pump(id: string): void {
    if (this.disposed) return
    const want = this.intent.get(id)
    if (want === undefined) return
    const current = this.data.threads.find((t) => t.id === id)
    if (!current || current.isResolved === want) {
      this.intent.delete(id)
      this.changed()
      return
    }
    this.inflight.add(id)
    this.changed()
    this.sendResolved(id, want).then(
      (thread) => {
        this.inflight.delete(id)
        this.settled.set(id, stamp())
        // Unless a refresh dropped the thread meanwhile.
        if (this.data.threads.some((t) => t.id === id)) {
          this.data = { ...this.data, threads: this.data.threads.map((t) => (t.id === id ? thread : t)) }
        }
        this.changed()
        this.pump(id)
      },
      (error: unknown) => {
        // GitHub refused (or the answer was lost, then the page reads the thread again): a read
        // sent meanwhile is still the newer version.
        this.inflight.delete(id)
        this.intent.delete(id)
        if (this.disposed) return
        this.changed()
        this.onError(id, error)
      },
    )
  }
}

/** What discarding the viewer's pending review `reviewId` takes: its comments, and the threads left without any. */
export function discarded(threads: readonly ReviewThread[], reviewId: string): { commentIds: string[]; threadIds: string[] } {
  const commentIds: string[] = []
  const threadIds: string[] = []
  for (const t of threads) {
    const pending = t.comments.filter((c) => inReview(c, reviewId))
    commentIds.push(...pending.map((c) => c.id))
    if (pending.length > 0 && pending.length === t.comments.length) threadIds.push(t.id)
  }
  return { commentIds, threadIds }
}

/** Ids of the viewer's own comments, taken before a create so its result can be told apart. */
export function ownCommentIds(threads: readonly ReviewThread[]): Set<string> {
  const ids = new Set<string>()
  for (const t of threads) for (const c of t.comments) if (c.viewerDidAuthor) ids.add(c.id)
  return ids
}

/** Where a comment was posted: in a thread, or as a new thread on a file or on lines. */
export type CreatedWhere =
  | { threadId: string }
  | { path: string; subjectType: 'FILE' }
  | { path: string; subjectType: 'LINE'; side: DiffSide; line: number }

function isWhere(t: ReviewThread, where: CreatedWhere): boolean {
  if ('threadId' in where) return t.id === where.threadId
  if (t.path !== where.path || t.subjectType !== where.subjectType) return false
  return where.subjectType === 'FILE' || (t.side === where.side && t.line === where.line)
}

/**
 * After a create whose outcome is unknown (a timeout or a 502 after the request went out):
 * the viewer's comment with the same body that wasn't there before, from a fresh read.
 * Creates are never retried blindly, so this is how a duplicate is avoided.
 */
export function findCreated(
  threads: readonly ReviewThread[],
  before: ReadonlySet<string>,
  body: string,
  where: CreatedWhere,
): { thread: ReviewThread; comment: ReviewComment } | null {
  const want = body.trim()
  for (const thread of threads) {
    if (!isWhere(thread, where)) continue
    for (const comment of thread.comments) {
      if (comment.viewerDidAuthor && !before.has(comment.id) && comment.body.trim() === want) return { thread, comment }
    }
  }
  return null
}
