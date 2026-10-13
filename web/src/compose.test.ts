import { describe, expect, it } from 'vitest'
import { ApiError, type CreatedThread, type NewThreadRequest, type PendingReview, type ReviewThread, type ReviewThreads } from './api'
import { postThread, type PostDeps } from './compose'
import { ReviewStore } from './review'
import { mkThread } from './test-fixtures'

const HEAD = 'a'.repeat(40)
const REVIEW: PendingReview = { id: 'R1', body: '', commit: HEAD, createdAt: '2026-10-13T00:00:00Z', url: 'u' }
const REQ: NewThreadRequest = { head: HEAD, path: 'src/a.ts', body: 'Nit: rename', mode: 'review', subjectType: 'FILE' }

function mine(body: string, over: Partial<ReviewThread> = {}): ReviewThread {
  const t = mkThread({ path: 'src/a.ts', ...over })
  return { ...t, comments: [{ ...t.comments[0], body, viewerDidAuthor: true, state: 'PENDING' }] }
}

function setup(create: (req: NewThreadRequest) => Promise<CreatedThread>, reads: (() => Promise<ReviewThreads>)[] = []) {
  const old = mine('Nit: rename')
  const store = new ReviewStore({ head: HEAD, locked: false, pendingReview: null, threads: [old] }, () => new Promise(() => {}), () => {})
  let rereads = 0
  const deps: PostDeps = {
    store,
    create,
    read: () => {
      const next = reads.shift()
      if (!next) throw new Error('unexpected read')
      return next()
    },
    reread: () => rereads++,
    settle: () => Promise.resolve(),
  }
  return { store, deps, old, rereads: () => rereads }
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

  it('says whether it is safe to send again', async () => {
    // Not there (the same text from before doesn't count).
    const { deps, old } = setup(
      () => Promise.reject(new ApiError(0, 'network', 'Failed to fetch')),
      [async () => ({ head: HEAD, locked: false, pendingReview: null, threads: [old] })],
    )
    await expect(postThread(deps, REQ)).rejects.toThrow(/isn't on GitHub, so you can send it again/)
    // Can't tell.
    const blind = setup(
      () => Promise.reject(new ApiError(504, 'http_504', '')),
      [() => Promise.reject(new ApiError(0, 'network', 'down'))],
    )
    await expect(postThread(blind.deps, REQ)).rejects.toThrow(/may have been posted all the same/)
  })
})
