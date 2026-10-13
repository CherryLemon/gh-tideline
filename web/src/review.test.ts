import { describe, expect, it, vi } from 'vitest'
import type { ReviewThread, ReviewThreads } from './api'
import { ReviewStore, discarded, findCreated, ownCommentIds, stamp } from './review'
import { mkThread } from './test-fixtures'

interface Call {
  id: string
  resolved: boolean
  resolve(t: ReviewThread): void
  reject(e: unknown): void
}

function setup(threads: ReviewThread[]) {
  const calls: Call[] = []
  const errors: string[] = []
  let notified = 0
  const initial: ReviewThreads = { head: 'h1', locked: false, latestReview: null, pendingReview: null, threads }
  const store = new ReviewStore(
    initial,
    (id, resolved) => new Promise<ReviewThread>((resolve, reject) => calls.push({ id, resolved, resolve, reject })),
    (id) => errors.push(id),
  )
  store.subscribe(() => notified++)
  const settle = () => new Promise((r) => setTimeout(r, 0))
  const thread = (id: string) => store.getSnapshot().threads.find((t) => t.id === id)
  return { store, calls, errors, notified: () => notified, settle, thread }
}

describe('ReviewStore resolve', () => {
  it('shows the change at once and keeps the server’s answer', async () => {
    const t = mkThread()
    const { store, calls, thread, settle } = setup([t])
    store.setResolved(t.id, true)
    expect(thread(t.id)).toMatchObject({ isResolved: true, isCollapsed: true, viewerCanUnresolve: true })
    expect(store.getSnapshot().busy.has(t.id)).toBe(true)
    expect(calls).toHaveLength(1)
    calls[0].resolve({ ...t, isResolved: true, resolvedBy: 'me', viewerCanResolve: false, viewerCanUnresolve: true })
    await settle()
    expect(thread(t.id)).toMatchObject({ isResolved: true, resolvedBy: 'me' })
    expect(store.getSnapshot().busy.size).toBe(0)
  })

  it('rolls back on failure and reports it', async () => {
    const t = mkThread()
    const { store, calls, thread, errors, settle } = setup([t])
    store.setResolved(t.id, true)
    calls[0].reject(new Error('422'))
    await settle()
    expect(thread(t.id)!.isResolved).toBe(false)
    expect(errors).toEqual([t.id])
    expect(store.getSnapshot().busy.size).toBe(0)
  })

  it('sends one request at a time and the latest intent last', async () => {
    const t = mkThread()
    const { store, calls, thread, settle } = setup([t])
    store.setResolved(t.id, true)
    store.setResolved(t.id, false)
    store.setResolved(t.id, true)
    store.setResolved(t.id, false)
    expect(calls).toHaveLength(1)
    expect(thread(t.id)!.isResolved).toBe(false)
    calls[0].resolve({ ...t, isResolved: true })
    await settle()
    expect(calls.map((c) => c.resolved)).toEqual([true, false])
    calls[1].resolve({ ...t, isResolved: false })
    await settle()
    expect(calls).toHaveLength(2)
    expect(thread(t.id)!.isResolved).toBe(false)
  })

  it('sends nothing when the intent matches the thread', () => {
    const t = mkThread({ isResolved: true })
    const { store, calls } = setup([t])
    store.setResolved(t.id, true)
    expect(calls).toHaveLength(0)
    expect(store.getSnapshot().busy.size).toBe(0)
  })
})

describe('ReviewStore rebase and writes', () => {
  it('says whether it has the thread of a comment', () => {
    const a = mkThread()
    const { store } = setup([a])
    expect(store.applyComment(a.id, { ...a.comments[0], body: 'edited' })).toBe(true)
    expect(store.getSnapshot().threads[0].comments.map((c) => c.body)).toEqual(['edited'])
    expect(store.applyComment('gone', { ...a.comments[0], id: 'x' })).toBe(false)
    expect(store.getSnapshot().threads[0].comments).toHaveLength(1)
    // An edit of a comment that is gone (deleted meanwhile) doesn't bring it back; a reply is added.
    expect(store.applyComment(a.id, { ...a.comments[0], id: 'x' }, true)).toBe(false)
    expect(store.getSnapshot().threads[0].comments).toHaveLength(1)
    expect(store.applyComment(a.id, { ...a.comments[0], id: 'x' })).toBe(true)
    expect(store.getSnapshot().threads[0].comments).toHaveLength(2)
  })

  it('takes a newer read', () => {
    const a = mkThread()
    const { store } = setup([a])
    const b = mkThread()
    store.rebase({ head: 'h2', locked: true, latestReview: null, pendingReview: null, threads: [b] }, stamp())
    const s = store.getSnapshot()
    expect(s.threads.map((t) => t.id)).toEqual([b.id])
    expect([s.head, s.locked]).toEqual(['h2', true])
  })

  it('keeps writes that settled after the read was requested', () => {
    const a = mkThread()
    const gone = mkThread()
    const { store } = setup([a, gone])
    const requestedAt = stamp()
    const created = mkThread()
    store.applyThread(created.id, created)
    store.applyComment(a.id, { ...a.comments[0], id: 'reply', body: 'r' })
    store.applyThread(gone.id, null)
    store.applyPendingReview({ id: 'P', body: '', commit: 'h1', createdAt: 'c', url: 'u' })
    // The read started before those writes reached GitHub.
    store.rebase({ head: 'h1', locked: false, latestReview: null, pendingReview: null, threads: [a, gone] }, requestedAt)
    const s = store.getSnapshot()
    expect(s.threads.map((t) => t.id)).toEqual([a.id, created.id])
    expect(s.threads[0].comments.map((c) => c.id)).toEqual([a.comments[0].id, 'reply'])
    expect(s.pendingReview?.id).toBe('P')
    // A read sent after the writes is taken as is.
    store.rebase({ head: 'h1', locked: false, latestReview: null, pendingReview: null, threads: [a] }, stamp())
    expect(store.getSnapshot().threads.map((t) => t.id)).toEqual([a.id])
    expect(store.getSnapshot().pendingReview).toBeNull()
  })

  it('takes a submitted review: no comment pending, no pending review, the latest review', () => {
    const mixed = mkThread()
    const pending = { ...mixed.comments[0], id: 'p1', state: 'PENDING' as const, viewerDidAuthor: true, reviewId: 'P' }
    const t = { ...mixed, comments: [mixed.comments[0], pending] }
    const other = mkThread()
    const { store } = setup([t, other])
    store.applyPendingReview({ id: 'P', body: '', commit: 'h1', createdAt: 'c', url: 'u' })
    expect(store.getSnapshot().pendingCount).toBe(1)
    // A review of just a summary leaves the pending one alone.
    const summary = { id: 'S', state: 'COMMENTED', url: 'u', body: 'Looks fine' }
    store.applySubmitted(summary, null)
    expect([store.getSnapshot().pendingCount, store.getSnapshot().pendingReview?.id, store.getSnapshot().latestReview]).toEqual([1, 'P', summary])
    const requestedAt = stamp()
    const latest = { id: 'P', state: 'APPROVED', url: 'u', body: 'LGTM' }
    store.applySubmitted(latest, 'P')
    let s = store.getSnapshot()
    expect(s.pendingCount).toBe(0)
    expect(s.pendingReview).toBeNull()
    expect(s.latestReview).toEqual(latest)
    expect(s.threads[0].comments.map((c) => c.state)).toEqual(['SUBMITTED', 'SUBMITTED'])
    expect(s.threads[1]).toBe(other)
    // A read sent before the submit landed still has it pending.
    const stale = { head: 'h1', locked: false, latestReview: null, pendingReview: { id: 'P', body: '', commit: 'h1', createdAt: 'c', url: 'u' } }
    store.rebase({ ...stale, threads: [t, other] }, requestedAt)
    s = store.getSnapshot()
    expect([s.pendingCount, s.pendingReview, s.latestReview]).toEqual([0, null, latest])
    // No answer: the latest review read stays.
    store.applySubmitted(null, 'P')
    expect(store.getSnapshot().latestReview).toEqual(latest)
  })

  it('submits or discards only the comments of the review given', () => {
    const t = mkThread()
    const mine = { ...t, comments: [{ ...t.comments[0], state: 'PENDING' as const, reviewId: 'P' }] }
    const u = mkThread()
    const next = { ...u, comments: [{ ...u.comments[0], state: 'PENDING' as const, reviewId: 'P2' }] }
    const P2 = { id: 'P2', body: '', commit: 'h1', createdAt: 'c', url: 'u' }
    // A comment's answer, in review P2 started after P was discarded, came first.
    const discarding = setup([mine, next]).store
    discarding.applyPendingReview(P2)
    discarding.applyDiscard('P')
    let s = discarding.getSnapshot()
    expect([s.threads.map((th) => th.id), s.pendingCount, s.pendingReview]).toEqual([[next.id], 1, P2])
    const submitting = setup([mine, next]).store
    submitting.applyPendingReview(P2)
    submitting.applySubmitted(null, 'P')
    s = submitting.getSnapshot()
    expect([s.threads.map((th) => th.comments[0].state), s.pendingReview]).toEqual([['SUBMITTED', 'PENDING'], P2])
  })

  it('takes a read sent right after a write, on a coarse clock too', () => {
    const t = mkThread()
    const { store, thread } = setup([t])
    const now = performance.now()
    const clock = vi.spyOn(performance, 'now').mockReturnValue(now)
    try {
      store.applyThread(t.id, { ...t, viewerCanResolve: false })
      // The read after a submit brings what the write's answer didn't have.
      store.rebase({ head: 'h1', locked: false, latestReview: null, pendingReview: null, threads: [{ ...t, viewerCanResolve: true }] }, stamp())
      expect(thread(t.id)!.viewerCanResolve).toBe(true)
    } finally {
      clock.mockRestore()
    }
  })

  it('drops a discarded review: its comments, and threads left without any', () => {
    const mixed = mkThread()
    const pending = { ...mixed.comments[0], id: 'p1', state: 'PENDING' as const, viewerDidAuthor: true, reviewId: 'P' }
    const t = { ...mixed, comments: [mixed.comments[0], pending] }
    const only = mkThread()
    const alone = { ...only, comments: [{ ...only.comments[0], state: 'PENDING' as const, reviewId: 'P' }] }
    const other = mkThread()
    const { store } = setup([t, alone, other])
    expect(discarded(store.getSnapshot().threads, 'P')).toEqual({ commentIds: ['p1', alone.comments[0].id], threadIds: [alone.id] })
    expect(discarded(store.getSnapshot().threads, 'P2')).toEqual({ commentIds: [], threadIds: [] })
    store.applyPendingReview({ id: 'P', body: '', commit: 'h1', createdAt: 'c', url: 'u' })
    const requestedAt = stamp()
    store.applyDiscard('P')
    let s = store.getSnapshot()
    expect(s.threads.map((th) => th.id)).toEqual([t.id, other.id])
    expect(s.threads[0].comments.map((c) => c.id)).toEqual([mixed.comments[0].id])
    expect([s.pendingCount, s.pendingReview]).toEqual([0, null])
    // A read sent before the discard landed doesn't bring them back.
    const stale = { head: 'h1', locked: false, latestReview: null, pendingReview: { id: 'P', body: '', commit: 'h1', createdAt: 'c', url: 'u' } }
    store.rebase({ ...stale, threads: [t, alone, other] }, requestedAt)
    s = store.getSnapshot()
    expect(s.threads.map((th) => th.id)).toEqual([t.id, other.id])
    expect([s.pendingCount, s.pendingReview]).toEqual([0, null])
  })

  it('ignores a read requested before one already taken', () => {
    const a = mkThread()
    const { store } = setup([a])
    const early = stamp()
    store.rebase({ head: 'h2', locked: false, latestReview: null, pendingReview: null, threads: [a] }, stamp())
    store.rebase({ head: 'h0', locked: false, latestReview: null, pendingReview: null, threads: [] }, early)
    expect(store.getSnapshot().head).toBe('h2')
  })

  it('keeps a thread with a resolve in flight through a read', async () => {
    const t = mkThread()
    const { store, calls, thread, settle } = setup([t])
    store.setResolved(t.id, true)
    store.rebase({ head: 'h1', locked: false, latestReview: null, pendingReview: null, threads: [{ ...t, isResolved: false }] }, stamp())
    expect(thread(t.id)!.isResolved).toBe(true)
    calls[0].resolve({ ...t, isResolved: true })
    await settle()
    expect(thread(t.id)!.isResolved).toBe(true)
  })

  it('takes the lines of a new head under a resolve in flight', () => {
    const t = mkThread({ line: 12 })
    const { store, thread } = setup([t])
    store.setResolved(t.id, true)
    store.rebase({ head: 'h2', locked: false, latestReview: null, pendingReview: null, threads: [{ ...t, line: 20 }] }, stamp())
    expect(thread(t.id)).toMatchObject({ line: 20, isResolved: true })
  })

  it('takes a read sent before a resolve failed', async () => {
    const t = mkThread({ line: 12 })
    const { store, calls, thread, settle } = setup([t])
    store.setResolved(t.id, true)
    const sentAt = stamp()
    calls[0].reject(new Error('nope'))
    await settle()
    store.rebase({ head: 'h2', locked: false, latestReview: null, pendingReview: null, threads: [{ ...t, line: 20 }] }, sentAt)
    expect(thread(t.id)).toMatchObject({ line: 20, isResolved: false })
  })

  it('drops a deleted comment from the stored thread, a resolve on its way kept apart', async () => {
    const t = mkThread()
    const two = { ...t, comments: [t.comments[0], { ...t.comments[0], id: 'C2' }] }
    const { store, calls, thread, settle } = setup([two])
    store.setResolved(t.id, true)
    expect(store.dropComment(t.id, 'C2', false)).toBe(true)
    expect(store.dropComment(t.id, 'C2', false)).toBe(false)
    expect(thread(t.id)).toMatchObject({ isResolved: true, comments: [t.comments[0]] })
    // GitHub refuses the resolve: the thread is as it was, less the comment.
    calls[0].reject(new Error('refused'))
    await settle()
    expect(thread(t.id)).toMatchObject({ isResolved: false, isCollapsed: false, comments: [t.comments[0]] })
    expect(store.dropComment(t.id, t.comments[0].id, true)).toBe(true)
    expect(store.getSnapshot().threads).toEqual([])
  })

  it('replaces an edited comment in place and counts pending comments', () => {
    const t = mkThread()
    const { store } = setup([t])
    expect(store.getSnapshot().pendingCount).toBe(0)
    store.applyComment(t.id, { ...t.comments[0], body: 'edited', state: 'PENDING' })
    const s = store.getSnapshot()
    expect(s.threads[0].comments.map((c) => c.body)).toEqual(['edited'])
    expect(s.pendingCount).toBe(1)
  })

  it('keeps the snapshot identity until something changes, and notifies', () => {
    const t = mkThread()
    const { store, notified } = setup([t])
    const s = store.getSnapshot()
    expect(store.getSnapshot()).toBe(s)
    store.applyThread(t.id, { ...t, isOutdated: true })
    expect(store.getSnapshot()).not.toBe(s)
    expect(notified()).toBe(1)
  })

  it('does nothing after dispose', () => {
    const t = mkThread()
    const { store, calls, notified } = setup([t])
    store.dispose()
    store.setResolved(t.id, true)
    store.applyThread(t.id, null)
    expect(calls).toHaveLength(0)
    expect(notified()).toBe(0)
  })
})

describe('findCreated', () => {
  it('finds the viewer’s new comment with the same body', () => {
    const mine = mkThread({ path: 'src/x.ts' })
    mine.comments[0] = { ...mine.comments[0], id: 'old', body: 'same', viewerDidAuthor: true }
    const before = ownCommentIds([mine])
    expect([...before]).toEqual(['old'])
    const after = { ...mine, comments: [...mine.comments, { ...mine.comments[0], id: 'new', body: ' same\n' }] }
    const at = { path: 'src/x.ts', subjectType: 'LINE', side: mine.side!, line: mine.line! } as const
    expect(findCreated([after], before, 'same', at)?.comment.id).toBe('new')
    expect(findCreated([after], before, 'same', { threadId: mine.id })?.comment.id).toBe('new')
    expect(findCreated([after], before, 'same', { ...at, path: 'src/y.ts' })).toBeNull()
    expect(findCreated([after], before, 'same', { ...at, line: at.line + 1 })).toBeNull()
    expect(findCreated([after], before, 'same', { path: 'src/x.ts', subjectType: 'FILE' })).toBeNull()
    expect(findCreated([after], before, 'other', at)).toBeNull()
    // Someone else's comment with the same text is not ours.
    const theirs = { ...mine, comments: [{ ...mine.comments[0], id: 'x', body: 'same', viewerDidAuthor: false }] }
    expect(findCreated([theirs], before, 'same', at)).toBeNull()
  })
})
