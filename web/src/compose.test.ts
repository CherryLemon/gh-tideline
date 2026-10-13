import { describe, expect, it } from 'vitest'
import { ApiError, type CreatedThread, type NewThreadRequest, type PendingReview, type ReviewThread, type ReviewThreads } from './api'
import { postThread, threadRequest, type PostDeps } from './compose'
import { ReviewStore } from './review'
import { mkThread } from './test-fixtures'

const HEAD = 'a'.repeat(40)
const REVIEW: PendingReview = { id: 'R1', body: '', commit: HEAD, createdAt: '2026-10-13T00:00:00Z', url: 'u' }
const REQ: NewThreadRequest = { head: HEAD, path: 'src/a.ts', body: 'Nit: rename', mode: 'review', subjectType: 'FILE' }

function mine(body: string, over: Partial<ReviewThread> = {}): ReviewThread {
  const t = mkThread({ path: 'src/a.ts', subjectType: 'FILE', side: null, line: null, ...over })
  return { ...t, comments: [{ ...t.comments[0], body, viewerDidAuthor: true, state: 'PENDING' }] }
}

function setup(create: (req: NewThreadRequest) => Promise<CreatedThread>, reads: (() => Promise<ReviewThreads>)[] = []) {
  const old = mine('Nit: rename')
  const store = new ReviewStore({ head: HEAD, locked: false, pendingReview: null, threads: [old] }, () => new Promise(() => {}), () => {})
  let rereads = 0
  const settles: number[] = []
  const deps: PostDeps = {
    store,
    create,
    read: () => {
      const next = reads.shift()
      if (!next) throw new Error('unexpected read')
      return next()
    },
    reread: () => rereads++,
    settle: (attempt) => {
      settles.push(attempt)
      return Promise.resolve()
    },
  }
  return { store, deps, old, rereads: () => rereads, settles }
}

describe('postThread', () => {
  it('takes the new thread and the pending review into the store', async () => {
    const created = mine('Nit: rename')
    const { store, deps } = setup(async () => ({ thread: created, pendingReview: REVIEW }))
    expect(await postThread(deps, REQ)).toEqual({})
    expect(store.getSnapshot().threads.map((t) => t.id)).toContain(created.id)
    expect(store.getSnapshot().pendingReview).toEqual(REVIEW)
  })

  it('reads again when the thread did not come back, and passes a submit error on', async () => {
    const { store, deps, rereads } = setup(async () => ({ thread: null, pendingReview: REVIEW, submitError: 'Something went wrong' }))
    expect(await postThread(deps, REQ)).toEqual({ submitError: 'Something went wrong' })
    expect(rereads()).toBe(1)
    expect(store.getSnapshot().pendingReview).toEqual(REVIEW)
  })

  it('passes refusals on without reading', async () => {
    const refused = new ApiError(422, 'rejected', 'no')
    const { deps } = setup(() => Promise.reject(refused))
    await expect(postThread(deps, REQ)).rejects.toBe(refused)
  })

  it('finds a comment that landed although the create failed, and does not post it twice', async () => {
    const landed = mine('Nit: rename ')
    const { store, deps, old } = setup(
      () => Promise.reject(new ApiError(502, 'github_error', 'timed out')),
      [async () => ({ head: HEAD, locked: false, pendingReview: REVIEW, threads: [old, landed] })],
    )
    expect(await postThread(deps, REQ)).toEqual({})
    expect(store.getSnapshot().threads.map((t) => t.id)).toEqual([old.id, landed.id])
    expect(store.getSnapshot().pendingReview).toEqual(REVIEW)
  })

  it('looks again before saying it is safe to send again', async () => {
    // Not there (the same text from before doesn't count), twice.
    const reads: (() => Promise<ReviewThreads>)[] = []
    const { deps, settles, old } = setup(() => Promise.reject(new ApiError(0, 'network', 'Failed to fetch')), reads)
    const empty = async () => ({ head: HEAD, locked: false, pendingReview: null, threads: [old] })
    reads.push(empty, empty)
    await expect(postThread(deps, REQ)).rejects.toThrow(/wasn't found on GitHub, so it most likely wasn't posted: you can send it again/)
    expect(settles).toEqual([0, 1])
    // There on the second look.
    const late = mine('Nit: rename')
    const second = setup(() => Promise.reject(new ApiError(502, 'github_error', 'timed out')), [
      async () => ({ head: HEAD, locked: false, pendingReview: null, threads: [] }),
      async () => ({ head: HEAD, locked: false, pendingReview: REVIEW, threads: [late] }),
    ])
    expect(await postThread(second.deps, REQ)).toEqual({})
    expect(second.store.getSnapshot().threads.map((t) => t.id)).toEqual([late.id])
    // Can't tell.
    const blind = setup(
      () => Promise.reject(new ApiError(504, 'http_504', '')),
      [() => Promise.reject(new ApiError(0, 'network', 'down'))],
    )
    await expect(postThread(blind.deps, REQ)).rejects.toThrow(/may have been posted all the same/)
  })

  it('says a single comment found still pending was not posted', async () => {
    const stuck = mine('Nit: rename')
    const { deps, old } = setup(
      () => Promise.reject(new ApiError(502, 'github_error', 'timed out')),
      [async () => ({ head: HEAD, locked: false, pendingReview: REVIEW, threads: [old, stuck] })],
    )
    const posted = await postThread(deps, { ...REQ, mode: 'single' })
    expect(posted.submitError).toMatch(/timed out/)
  })

  it('matches a comment on lines by its side and line', async () => {
    const req: NewThreadRequest = { ...REQ, subjectType: 'LINE', side: 'RIGHT', line: 7 }
    const elsewhere = mine('Nit: rename', { subjectType: 'LINE', side: 'RIGHT', line: 8 })
    const there = mine('Nit: rename', { subjectType: 'LINE', side: 'RIGHT', line: 7 })
    const { deps, old } = setup(() => Promise.reject(new ApiError(502, 'github_error', 'timed out')), [
      async () => ({ head: HEAD, locked: false, pendingReview: REVIEW, threads: [old, elsewhere] }),
      async () => ({ head: HEAD, locked: false, pendingReview: REVIEW, threads: [old, elsewhere, there] }),
    ])
    expect(await postThread(deps, req)).toEqual({})
  })
})

describe('threadRequest', () => {
  const OLD = 'b'.repeat(40)
  it('sends lines at the head they were picked at, a file at the page head', () => {
    expect(threadRequest({ target: { kind: 'file', path: 'a.ts' }, head: OLD }, HEAD, 'x', 'single')).toEqual({
      subjectType: 'FILE',
      path: 'a.ts',
      head: HEAD,
      body: 'x',
      mode: 'single',
    })
    expect(threadRequest({ target: { kind: 'lines', path: 'a.ts', side: 'LEFT', startLine: 3, line: 5 }, head: OLD }, HEAD, 'x', 'review')).toEqual({
      subjectType: 'LINE',
      path: 'a.ts',
      side: 'LEFT',
      line: 5,
      startLine: 3,
      startSide: 'LEFT',
      head: OLD,
      body: 'x',
      mode: 'review',
    })
    const one = threadRequest({ target: { kind: 'lines', path: 'a.ts', side: 'RIGHT', startLine: null, line: 5 }, head: HEAD }, HEAD, 'x', 'review')
    expect(one).not.toHaveProperty('startLine')
    expect(one).not.toHaveProperty('startSide')
    expect(threadRequest({ target: { kind: 'review' }, head: HEAD }, HEAD, 'x', 'review')).toBeNull()
  })
})
