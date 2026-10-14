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
  type SubmitRequest,
  type SubmittedReview,
} from './api'
import {
  postDelete,
  postDiscard,
  postReply,
  postSubmit,
  postThread,
  threadRequest,
  type DeleteDeps,
  type DiscardDeps,
  type PostDeps,
  type ReplyDeps,
  type SubmitDeps,
} from './compose'
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
  const store = new ReviewStore({ head: HEAD, locked: false, latestReview: null, pendingReview: null, threads: [old] }, () => new Promise(() => {}), () => {})
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
      [async () => ({ head: HEAD, locked: false, latestReview: null, pendingReview: REVIEW, threads: [old, landed] })],
    )
    expect(await postThread(deps, REQ)).toEqual({})
    expect(store.getSnapshot().threads.map((t) => t.id)).toEqual([old.id, landed.id])
    expect(store.getSnapshot().pendingReview).toEqual(REVIEW)
  })

  it('looks again before saying it is safe to send again', async () => {
    // Not there (the same text from before doesn't count), twice.
    const reads: (() => Promise<ReviewThreads>)[] = []
    const { deps, settles, old } = setup(() => Promise.reject(new ApiError(0, 'network', 'Failed to fetch')), reads)
    const empty = async () => ({ head: HEAD, locked: false, latestReview: null, pendingReview: null, threads: [old] })
    reads.push(empty, empty)
    await expect(postThread(deps, REQ)).rejects.toThrow(/wasn't found on GitHub, so it most likely wasn't posted: you can send it again/)
    expect(settles).toEqual([0, 1])
    // There on the second look.
    const late = mine('Nit: rename')
    const second = setup(() => Promise.reject(new ApiError(502, 'github_error', 'timed out')), [
      async () => ({ head: HEAD, locked: false, latestReview: null, pendingReview: null, threads: [] }),
      async () => ({ head: HEAD, locked: false, latestReview: null, pendingReview: REVIEW, threads: [late] }),
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
      [async () => ({ head: HEAD, locked: false, latestReview: null, pendingReview: REVIEW, threads: [old, stuck] })],
    )
    const posted = await postThread(deps, { ...REQ, mode: 'single' })
    expect(posted.submitError).toMatch(/timed out/)
  })

  it('matches a comment on lines by its side and line', async () => {
    const req: NewThreadRequest = { ...REQ, subjectType: 'LINE', side: 'RIGHT', line: 7 }
    const elsewhere = mine('Nit: rename', { subjectType: 'LINE', side: 'RIGHT', line: 8 })
    const there = mine('Nit: rename', { subjectType: 'LINE', side: 'RIGHT', line: 7 })
    const { deps, old } = setup(() => Promise.reject(new ApiError(502, 'github_error', 'timed out')), [
      async () => ({ head: HEAD, locked: false, latestReview: null, pendingReview: REVIEW, threads: [old, elsewhere] }),
      async () => ({ head: HEAD, locked: false, latestReview: null, pendingReview: REVIEW, threads: [old, elsewhere, there] }),
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
    const store = new ReviewStore({ head: HEAD, locked: false, latestReview: null, pendingReview, threads: [thread] }, () => new Promise(() => {}), () => {})
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
      [async () => ({ head: HEAD, locked: false, latestReview: null, pendingReview: REVIEW, threads: [landed] })],
    )
    // The store's thread is another one than `landed`: only the read's counts.
    await postReply(deps, landed.id, ASK)
    expect(store.getSnapshot().threads.map((t) => t.id)).toEqual([landed.id])
    expect(store.getSnapshot().pendingReview).toEqual(REVIEW)
    // The same text in another thread is not it.
    const other = mkThread()
    const elsewhere = { ...other, comments: [...other.comments, replyTo(other, 'Agreed')] }
    const missed = setupReply(() => Promise.reject(new ApiError(502, 'github_error', 'timed out')), [
      async () => ({ head: HEAD, locked: false, latestReview: null, pendingReview: null, threads: [elsewhere] }),
      async () => ({ head: HEAD, locked: false, latestReview: null, pendingReview: null, threads: [elsewhere] }),
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
    const initial: ReviewThreads = { head: HEAD, locked: false, latestReview: null, pendingReview: REVIEW, threads: [two] }
    const store = new ReviewStore(initial, () => new Promise(() => {}), () => {})
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
    const read = (threads: ReviewThread[]) => async () => ({ head: HEAD, locked: false, latestReview: null, pendingReview: null, threads })
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

/** A store with one thread of a submitted and a pending comment, and the pending review. */
function reviewSetup(latestReview: SubmittedReview | null = null) {
  const thread = mkThread()
  const pending = { ...thread.comments[0], id: `${thread.id}-2`, viewerDidAuthor: true, state: 'PENDING' as const, reviewId: REVIEW.id }
  const two = { ...thread, comments: [thread.comments[0], pending] }
  const store = new ReviewStore({ head: HEAD, locked: false, latestReview, pendingReview: REVIEW, threads: [two] }, () => new Promise(() => {}), () => {})
  const reads: (() => Promise<ReviewThreads>)[] = []
  const settles: number[] = []
  let rereads = 0
  const read: SubmitDeps['read'] = () => {
    const next = reads.shift()
    if (!next) throw new Error('unexpected read')
    return next()
  }
  const settle = (attempt: number) => {
    settles.push(attempt)
    return Promise.resolve()
  }
  const answer = (over: Partial<ReviewThreads>) => async (): Promise<ReviewThreads> => ({
    head: HEAD,
    locked: false,
    latestReview,
    pendingReview: REVIEW,
    threads: [two],
    ...over,
  })
  return { store, thread, two, reads, settles, read, settle, answer, rereads: () => rereads, reread: () => rereads++ }
}

describe('postSubmit', () => {
  const SUBMITTED: SubmittedReview = { id: REVIEW.id, state: 'APPROVED', url: 'u', body: 'LGTM' }
  const WITH_REVIEW: SubmitRequest = { head: HEAD, reviewId: REVIEW.id, event: 'APPROVE', body: 'LGTM' }
  const SUMMARY_ONLY: SubmitRequest = { head: HEAD, reviewId: null, event: 'REQUEST_CHANGES', body: 'Please split this\r\n' }
  const lost = new ApiError(502, 'github_error', 'GraphQL request to github.com failed: operation timed out')

  function setupSubmit(submit: SubmitDeps['submit'], latest: SubmittedReview | null = null) {
    const r = reviewSetup(latest)
    const deps: SubmitDeps = { store: r.store, submit, read: r.read, reread: r.reread, settle: r.settle }
    return { ...r, deps }
  }

  it('takes the submitted review and reads the rest again', async () => {
    const { store, deps, rereads } = setupSubmit(async () => SUBMITTED)
    expect(await postSubmit(deps, WITH_REVIEW)).toEqual(SUBMITTED)
    const s = store.getSnapshot()
    expect([s.pendingReview, s.pendingCount, s.latestReview]).toEqual([null, 0, SUBMITTED])
    expect(rereads()).toBe(1)
  })

  it('passes refusals on without reading', async () => {
    const refused = new ApiError(422, 'rejected', 'Can not approve your own pull request')
    const { store, deps, settles } = setupSubmit(() => Promise.reject(refused))
    await expect(postSubmit(deps, WITH_REVIEW)).rejects.toBe(refused)
    expect(settles).toEqual([])
    expect(store.getSnapshot().pendingCount).toBe(1)
  })

  it('finds a pending review submitted although the answer was lost, by its id', async () => {
    const { store, deps, reads, answer, settles } = setupSubmit(() => Promise.reject(lost))
    // Not yet on the first look; on the second, whatever its state.
    reads.push(answer({}), answer({ pendingReview: null, latestReview: { ...SUBMITTED, state: 'COMMENTED', body: '' } }))
    expect((await postSubmit(deps, WITH_REVIEW))?.id).toBe(REVIEW.id)
    expect(settles).toEqual([0, 1])
    expect(store.getSnapshot().pendingCount).toBe(0)
  })

  it('finds a summary submitted on its own by its state and text, not by a new id alone', async () => {
    const before: SubmittedReview = { id: 'R0', state: 'CHANGES_REQUESTED', url: 'u0', body: 'Please split this' }
    // A single comment posted meanwhile made a review too.
    const single: SubmittedReview = { id: 'R1', state: 'COMMENTED', url: 'u1', body: '' }
    const mine: SubmittedReview = { id: 'R2', state: 'CHANGES_REQUESTED', url: 'u2', body: 'Please split this' }
    const found = setupSubmit(() => Promise.reject(lost), before)
    found.reads.push(found.answer({ latestReview: before }), found.answer({ latestReview: mine }))
    expect(await postSubmit(found.deps, SUMMARY_ONLY)).toEqual(mine)
    expect(found.store.getSnapshot().latestReview).toEqual(mine)
    const not = setupSubmit(() => Promise.reject(lost), before)
    not.reads.push(not.answer({ latestReview: single }), not.answer({ latestReview: single }))
    await expect(postSubmit(not.deps, SUMMARY_ONLY)).rejects.toThrow(/wasn't found on GitHub, so it most likely wasn't submitted/)
    expect(not.store.getSnapshot().pendingCount).toBe(1)
    const unknown = setupSubmit(() => Promise.reject(lost))
    unknown.reads.push(() => Promise.reject(new ApiError(0, 'network', 'Failed to fetch')))
    await expect(postSubmit(unknown.deps, WITH_REVIEW)).rejects.toThrow(/may have been submitted all the same/)
  })

  it('finds a pending review submitted by its comments, when it is not the latest review', async () => {
    const { store, deps, reads, answer, two, rereads } = setupSubmit(() => Promise.reject(lost))
    const later: SubmittedReview = { id: 'R9', state: 'COMMENTED', url: 'u9', body: '' }
    const submitted = { ...two, comments: two.comments.map((c) => ({ ...c, state: 'SUBMITTED' as const })) }
    reads.push(answer({ pendingReview: null, latestReview: later, threads: [submitted] }))
    expect(await postSubmit(deps, WITH_REVIEW)).toBeNull()
    expect([store.getSnapshot().pendingCount, store.getSnapshot().pendingReview, rereads()]).toEqual([0, null, 1])
  })

  it('says a pending review gone but not found submitted is no longer pending', async () => {
    const { store, deps, reads, answer, thread } = setupSubmit(() => Promise.reject(lost))
    reads.push(answer({ pendingReview: null, threads: [thread] }), answer({ pendingReview: null, threads: [thread] }))
    await expect(postSubmit(deps, WITH_REVIEW)).rejects.toThrow(/no longer pending on GitHub: refresh to see whether it was submitted/)
    expect(store.getSnapshot().pendingReview).toBeNull()
  })
})

describe('postDiscard', () => {
  const lost = new ApiError(502, 'github_error', 'GraphQL request to github.com failed: operation timed out')

  function setupDiscard(discard: DiscardDeps['discard']) {
    const r = reviewSetup()
    const gone: [string[], number][] = []
    const deps: DiscardDeps = {
      store: r.store,
      discard,
      // With how many comments the store had then: before it drops them.
      gone: (what) => gone.push([what.commentIds, r.store.getSnapshot().threads[0]?.comments.length ?? 0]),
      read: r.read,
      reread: r.reread,
      settle: r.settle,
    }
    return { ...r, deps, gone }
  }

  it('drops the pending comments once GitHub has discarded them, and reads the rest again', async () => {
    const { store, deps, gone, two, rereads } = setupDiscard(async () => {})
    await postDiscard(deps, REVIEW.id)
    expect(gone).toEqual([[[two.comments[1].id], 2]])
    const s = store.getSnapshot()
    expect([s.pendingReview, s.pendingCount, s.threads[0].comments.length, rereads()]).toEqual([null, 0, 1, 1])
  })

  it('keeps a pending review started after the discard, whose answer came first', async () => {
    const { store, deps } = setupDiscard(async () => {})
    const next: PendingReview = { ...REVIEW, id: 'R2' }
    const t = mkThread()
    store.applyThread(t.id, { ...t, comments: [{ ...t.comments[0], state: 'PENDING', reviewId: next.id }] })
    store.applyPendingReview(next)
    await postDiscard(deps, REVIEW.id)
    const s = store.getSnapshot()
    expect([s.pendingReview, s.pendingCount, s.threads.map((th) => th.id)]).toEqual([next, 1, [s.threads[0].id, t.id]])
  })

  it('reads whether a discard whose answer was lost landed', async () => {
    const done = setupDiscard(() => Promise.reject(lost))
    done.reads.push(done.answer({}), done.answer({ pendingReview: null, threads: [done.thread] }))
    await postDiscard(done.deps, REVIEW.id)
    expect(done.settles).toEqual([0, 1])
    expect(done.gone).toEqual([[[done.two.comments[1].id], 2]])
    expect([done.store.getSnapshot().pendingCount, done.rereads()]).toEqual([0, 0])
    const kept = setupDiscard(() => Promise.reject(lost))
    kept.reads.push(kept.answer({}), kept.answer({}))
    await expect(postDiscard(kept.deps, REVIEW.id)).rejects.toThrow(/timed out\. It's still on GitHub: you can discard it again/)
    expect(kept.gone).toEqual([])
    expect(kept.store.getSnapshot().pendingCount).toBe(1)
    const refused = setupDiscard(() => Promise.reject(new ApiError(409, 'review_changed', 'not the one shown')))
    await expect(postDiscard(refused.deps, REVIEW.id)).rejects.toThrow(/changed on GitHub/)
    expect(refused.settles).toEqual([])
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
