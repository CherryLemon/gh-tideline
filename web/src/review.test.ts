import { describe, expect, it } from 'vitest'
import type { ReviewThread, ReviewThreads } from './api'
import { ReviewStore, findCreated, ownCommentIds } from './review'
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
  const initial: ReviewThreads = { head: 'h1', locked: false, pendingReview: null, threads }
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
  it('takes a newer read', () => {
    const a = mkThread()
    const { store } = setup([a])
    const b = mkThread()
    store.rebase({ head: 'h2', locked: true, pendingReview: null, threads: [b] }, performance.now())
    const s = store.getSnapshot()
    expect(s.threads.map((t) => t.id)).toEqual([b.id])
    expect([s.head, s.locked]).toEqual(['h2', true])
  })

  it('keeps writes that settled after the read was requested', () => {
    const a = mkThread()
    const gone = mkThread()
    const { store } = setup([a, gone])
    const requestedAt = performance.now()
    const created = mkThread()
    store.applyThread(created.id, created)
    store.applyComment(a.id, { ...a.comments[0], id: 'reply', body: 'r' })
    store.applyThread(gone.id, null)
    store.applyPendingReview({ id: 'P', body: '', commit: 'h1', createdAt: 'c', url: 'u' })
    // The read started before those writes reached GitHub.
    store.rebase({ head: 'h1', locked: false, pendingReview: null, threads: [a, gone] }, requestedAt)
    const s = store.getSnapshot()
    expect(s.threads.map((t) => t.id)).toEqual([a.id, created.id])
    expect(s.threads[0].comments.map((c) => c.id)).toEqual([a.comments[0].id, 'reply'])
    expect(s.pendingReview?.id).toBe('P')
    // A read sent after the writes is taken as is.
    store.rebase({ head: 'h1', locked: false, pendingReview: null, threads: [a] }, performance.now())
    expect(store.getSnapshot().threads.map((t) => t.id)).toEqual([a.id])
    expect(store.getSnapshot().pendingReview).toBeNull()
  })

  it('ignores a read requested before one already taken', () => {
    const a = mkThread()
    const { store } = setup([a])
    const early = performance.now()
    store.rebase({ head: 'h2', locked: false, pendingReview: null, threads: [a] }, performance.now())
    store.rebase({ head: 'h0', locked: false, pendingReview: null, threads: [] }, early)
    expect(store.getSnapshot().head).toBe('h2')
  })

  it('keeps a thread with a resolve in flight through a read', async () => {
    const t = mkThread()
    const { store, calls, thread, settle } = setup([t])
    store.setResolved(t.id, true)
    store.rebase({ head: 'h1', locked: false, pendingReview: null, threads: [{ ...t, isResolved: false }] }, performance.now())
    expect(thread(t.id)!.isResolved).toBe(true)
    calls[0].resolve({ ...t, isResolved: true })
    await settle()
    expect(thread(t.id)!.isResolved).toBe(true)
  })

  it('takes the lines of a new head under a resolve in flight', () => {
    const t = mkThread({ line: 12 })
    const { store, thread } = setup([t])
    store.setResolved(t.id, true)
    store.rebase({ head: 'h2', locked: false, pendingReview: null, threads: [{ ...t, line: 20 }] }, performance.now())
    expect(thread(t.id)).toMatchObject({ line: 20, isResolved: true })
  })

  it('takes a read sent before a resolve failed', async () => {
    const t = mkThread({ line: 12 })
    const { store, calls, thread, settle } = setup([t])
    store.setResolved(t.id, true)
    const sentAt = performance.now()
    calls[0].reject(new Error('nope'))
    await settle()
    store.rebase({ head: 'h2', locked: false, pendingReview: null, threads: [{ ...t, line: 20 }] }, sentAt)
    expect(thread(t.id)).toMatchObject({ line: 20, isResolved: false })
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
