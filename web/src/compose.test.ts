import { describe, expect, it } from 'vitest'
import {
  ApiError,
  type CreatedThread,
  type DeletedComment,
  type NewThreadRequest,
  type PendingReview,
  type Replied,
  type ReplyRequest,
  type ReviewComment,
  type ReviewThread,
  type ReviewThreads,
} from './api'
import { postDelete, postReply, postThread, threadRequest, type DeleteDeps, type PostDeps, type ReplyDeps } from './compose'
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

describe('postReply', () => {
  const ASK: ReplyRequest = { head: HEAD, body: 'Agreed', mode: 'review' }

  function replyTo(thread: ReviewThread, body: string, state: ReviewComment['state'] = 'PENDING'): ReviewComment {
    return { ...thread.comments[0], id: `${thread.id}-reply-${body}`, body, state, viewerDidAuthor: true }
  }

  function setupReply(reply: ReplyDeps['reply'], reads: (() => Promise<ReviewThreads>)[] = [], pendingReview: PendingReview | null = null) {
    const thread = mkThread()
    const store = new ReviewStore({ head: HEAD, locked: false, pendingReview, threads: [thread] }, () => new Promise(() => {}), () => {})
    let rereads = 0
    const deps: ReplyDeps = {
      store,
      reply,
      read: () => {
        const next = reads.shift()
        if (!next) throw new Error('unexpected read')
        return next()
      },
      reread: () => rereads++,
      settle: () => Promise.resolve(),
    }
    return { store, deps, thread, rereads: () => rereads }
  }

  it('adds the reply to its thread and takes the pending review', async () => {
    let sent: [string, ReplyRequest] | null = null
    const { store, deps, thread, rereads } = setupReply(async (id, req) => {
      sent = [id, req]
      return { comment: replyTo(thread, 'Agreed'), pendingReview: REVIEW }
    })
    await postReply(deps, thread.id, ASK)
    expect(sent).toEqual([thread.id, ASK])
    expect(store.getSnapshot().threads[0].comments.map((c) => c.body)).toEqual(['b', 'Agreed'])
    expect(store.getSnapshot().pendingReview).toEqual(REVIEW)
    expect(rereads()).toBe(0)
  })

  it('reads again when the reply or its thread is missing', async () => {
    const lost = setupReply(async () => ({ comment: null, pendingReview: REVIEW }))
    await postReply(lost.deps, lost.thread.id, ASK)
    expect(lost.rereads()).toBe(1)
    expect(lost.store.getSnapshot().pendingReview).toEqual(REVIEW)
    // Nor which review a single reply went into: the one the page has stays until the read.
    const unknown = setupReply(async () => ({ comment: null, pendingReview: null }), [], REVIEW)
    await postReply(unknown.deps, unknown.thread.id, { ...ASK, mode: 'single' })
    expect(unknown.rereads()).toBe(1)
    expect(unknown.store.getSnapshot().pendingReview).toEqual(REVIEW)
    // The thread went from the page meanwhile.
    const gone = setupReply(async () => ({ comment: replyTo(mkThread(), 'Agreed'), pendingReview: null }))
    await postReply(gone.deps, 'elsewhere', ASK)
    expect(gone.rereads()).toBe(1)
  })

  it('does not drop a pending review a single reply landed in', async () => {
    // Started on github.com after the page read the threads; the server couldn't say which.
    const answer: Replied = { comment: null, pendingReview: null }
    const { store, deps, thread, rereads } = setupReply(async () => answer, [], REVIEW)
    answer.comment = replyTo(thread, 'Agreed', 'PENDING')
    await postReply(deps, thread.id, { ...ASK, mode: 'single' })
    expect(rereads()).toBe(1)
    expect(store.getSnapshot().pendingReview).toEqual(REVIEW)
    // Shown meanwhile.
    expect(store.getSnapshot().threads[0].comments.map((c) => c.body)).toEqual(['b', 'Agreed'])
  })

  it('finds a reply that landed although the request failed', async () => {
    const thread = mkThread()
    const landed = { ...thread, comments: [...thread.comments, replyTo(thread, 'Agreed ')] }
    const { store, deps } = setupReply(
      () => Promise.reject(new ApiError(0, 'network', 'Failed to fetch')),
      [async () => ({ head: HEAD, locked: false, pendingReview: REVIEW, threads: [landed] })],
    )
    // The store's thread is another one than `landed`: only the read's counts.
    await postReply(deps, landed.id, ASK)
    expect(store.getSnapshot().threads.map((t) => t.id)).toEqual([landed.id])
    expect(store.getSnapshot().pendingReview).toEqual(REVIEW)
    // The same text in another thread is not it.
    const other = mkThread()
    const elsewhere = { ...other, comments: [...other.comments, replyTo(other, 'Agreed')] }
    const missed = setupReply(() => Promise.reject(new ApiError(502, 'github_error', 'timed out')), [
      async () => ({ head: HEAD, locked: false, pendingReview: null, threads: [elsewhere] }),
      async () => ({ head: HEAD, locked: false, pendingReview: null, threads: [elsewhere] }),
    ])
    await expect(postReply(missed.deps, missed.thread.id, ASK)).rejects.toThrow(/wasn't found on GitHub/)
  })

  it('passes refusals on without reading', async () => {
    const refused = new ApiError(404, 'not_found', 'this thread is no longer on GitHub')
    const { deps, thread, rereads } = setupReply(() => Promise.reject(refused))
    await expect(postReply(deps, thread.id, ASK)).rejects.toBe(refused)
    expect(rereads()).toBe(0)
  })
})

describe('postDelete', () => {
  function setupDelete(remove: DeleteDeps['remove'], reads: (() => Promise<ReviewThreads>)[] = []) {
    const thread = mkThread()
    const reply = { ...thread.comments[0], id: `${thread.id}-2`, viewerDidAuthor: true, state: 'PENDING' as const }
    const two = { ...thread, comments: [thread.comments[0], reply] }
    const store = new ReviewStore({ head: HEAD, locked: false, pendingReview: REVIEW, threads: [two] }, () => new Promise(() => {}), () => {})
    const settles: number[] = []
    const gone: [boolean, number][] = []
    const deps: DeleteDeps = {
      store,
      remove,
      // With how many comments the store had then: before it drops the comment.
      gone: (threadGone) => gone.push([threadGone, store.getSnapshot().threads[0]?.comments.length ?? 0]),
      read: () => {
        const next = reads.shift()
        if (!next) throw new Error('unexpected read')
        return next()
      },
      reread: () => {
        throw new Error('unexpected reread')
      },
      settle: (attempt) => {
        settles.push(attempt)
        return Promise.resolve()
      },
    }
    return { store, deps, thread, reply, two, settles, gone }
  }

  it("takes GitHub's answer: the thread without the comment, the pending review", async () => {
    const { store, deps, thread, reply, gone } = setupDelete(async () => ({ thread, pendingReview: null }))
    expect(await postDelete(deps, thread.id, reply.id)).toEqual({ threadGone: false })
    expect(gone).toEqual([[false, 2]])
    expect(store.getSnapshot().threads[0].comments.map((c) => c.id)).toEqual([thread.comments[0].id])
    expect(store.getSnapshot().pendingReview).toBeNull()
    const last = setupDelete(async (): Promise<DeletedComment> => ({ thread: null, pendingReview: REVIEW }))
    expect(await postDelete(last.deps, last.thread.id, last.reply.id)).toEqual({ threadGone: true })
    expect(last.store.getSnapshot().threads).toEqual([])
  })

  it('reads whether a delete whose answer was lost landed', async () => {
    const read = (threads: ReviewThread[]) => async () => ({ head: HEAD, locked: false, pendingReview: null, threads })
    const lost = new ApiError(502, 'github_error', 'the comment was deleted, but reading its thread back failed: timed out')
    const gone = setupDelete(() => Promise.reject(lost), [read([])])
    expect(await postDelete(gone.deps, gone.thread.id, gone.reply.id)).toEqual({ threadGone: true })
    expect(gone.settles).toEqual([0])
    expect(gone.gone).toEqual([[true, 2]])
    expect(gone.store.getSnapshot().threads).toEqual([])
    const landed = setupDelete(() => Promise.reject(lost), [])
    landed.deps.read = read([landed.thread])
    expect(await postDelete(landed.deps, landed.thread.id, landed.reply.id)).toEqual({ threadGone: false })
    expect(landed.store.getSnapshot().threads[0].comments).toHaveLength(1)
    const still = setupDelete(() => Promise.reject(lost), [])
    still.deps.read = read([still.two])
    await expect(postDelete(still.deps, still.thread.id, still.reply.id)).rejects.toBe(lost)
    expect(still.gone).toEqual([])
    // An edit of the other comment settled while the read was out: the store keeps its version,
    // less the deleted comment.
    const newer = setupDelete(() => Promise.reject(lost), [])
    const edited = { ...newer.two, comments: [{ ...newer.two.comments[0], body: 'edited' }, newer.reply] }
    newer.deps.read = async () => {
      newer.store.applyThread(newer.thread.id, edited)
      return read([newer.thread])()
    }
    expect(await postDelete(newer.deps, newer.thread.id, newer.reply.id)).toEqual({ threadGone: false })
    expect(newer.store.getSnapshot().threads[0].comments.map((c) => c.body)).toEqual(['edited'])
    const newerGone = setupDelete(() => Promise.reject(lost), [])
    newerGone.deps.read = async () => {
      newerGone.store.applyThread(newerGone.thread.id, newerGone.two)
      return read([])()
    }
    expect(await postDelete(newerGone.deps, newerGone.thread.id, newerGone.reply.id)).toEqual({ threadGone: true })
    expect(newerGone.store.getSnapshot().threads).toEqual([])
    const unknown = setupDelete(() => Promise.reject(lost), [() => Promise.reject(new ApiError(0, 'network', 'Failed to fetch'))])
    await expect(postDelete(unknown.deps, unknown.thread.id, unknown.reply.id)).rejects.toThrow(/may have been deleted all the same/)
  })

  it('passes refusals on without reading', async () => {
    const refused = new ApiError(422, 'rejected', 'GitHub refused')
    const { deps, thread, reply, settles } = setupDelete(() => Promise.reject(refused))
    await expect(postDelete(deps, thread.id, reply.id)).rejects.toBe(refused)
    expect(settles).toEqual([])
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
