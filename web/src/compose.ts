// Posting a new review thread or a reply, or deleting a comment, and taking GitHub's answer
// into the review store. Writes are never retried: when the outcome is unknown (the server or
// GitHub failed after the request went out), a fresh read tells whether the write landed.
import type { CommentMode, CreatedThread, DeletedComment, NewThreadRequest, Replied, ReplyRequest, ReviewComment, ReviewThreads } from './api'
import type { DraftView } from './drafts'
import { errorMessage, mayHaveLanded } from './api'
import { findCreated, ownCommentIds, type CreatedWhere, type ReviewStore } from './review'

/** What looking for a comment whose create failed needs. */
export interface ReadDeps {
  store: ReviewStore
  /** A fresh read of the threads. */
  read(): Promise<ReviewThreads>
  /** Reads again through the page (which reports a failure itself). */
  reread(): void
  /**
   * Before each look (`attempt` 0, 1) for a comment whose create failed: GitHub may still be
   * storing it.
   */
  settle(attempt: number): Promise<void>
}

export interface PostDeps extends ReadDeps {
  create(req: NewThreadRequest): Promise<CreatedThread>
}

export interface ReplyDeps extends ReadDeps {
  reply(threadId: string, req: ReplyRequest): Promise<Replied>
}

export interface DeleteDeps extends ReadDeps {
  remove(threadId: string, commentId: string): Promise<DeletedComment>
  /** Once the comment is known to be gone, before the store drops it (so its boxes go first). */
  gone(threadGone: boolean): void
}

export interface Posted {
  /** The comment was added, but the review that should have posted it is still pending. */
  submitError?: string
}

/**
 * The request for a draft of a new thread; null for other drafts. A comment on lines is sent
 * at the head its lines were picked at (GitHub refuses it if the pull request has moved on
 * since), a comment on the file at the head the page shows.
 */
export function threadRequest(draft: Pick<DraftView, 'target' | 'head'>, pageHead: string, body: string, mode: CommentMode): NewThreadRequest | null {
  const t = draft.target
  if (t.kind === 'file') return { subjectType: 'FILE', path: t.path, head: pageHead, body, mode }
  if (t.kind !== 'lines') return null
  const range = t.startLine !== null && t.startLine < t.line ? { startLine: t.startLine, startSide: t.side } : {}
  return { subjectType: 'LINE', path: t.path, side: t.side, line: t.line, ...range, head: draft.head, body, mode }
}

/** Posts `req`; resolves once the store has the new thread (or a read is on its way). */
export async function postThread(deps: PostDeps, req: NewThreadRequest): Promise<Posted> {
  const before = ownCommentIds(deps.store.getSnapshot().threads)
  let created: CreatedThread
  try {
    created = await deps.create(req)
  } catch (e) {
    if (!mayHaveLanded(e)) throw e
    const where: CreatedWhere =
      req.subjectType === 'FILE'
        ? { path: req.path, subjectType: 'FILE' }
        : { path: req.path, subjectType: 'LINE', side: req.side, line: req.line }
    const found = await lookForCreated(deps, e, before, req.body, where)
    // A single comment whose review couldn't be submitted waits in a pending review.
    return req.mode === 'single' && found.state === 'PENDING' ? { submitError: errorMessage(e) } : {}
  }
  deps.store.applyPendingReview(created.pendingReview)
  if (created.thread) deps.store.applyThread(created.thread.id, created.thread)
  else deps.reread()
  return created.submitError ? { submitError: created.submitError } : {}
}

/** Posts a reply to `threadId`; resolves once the store has it (or a read is on its way). */
export async function postReply(deps: ReplyDeps, threadId: string, req: ReplyRequest): Promise<void> {
  const before = ownCommentIds(deps.store.getSnapshot().threads)
  let replied: Replied
  try {
    replied = await deps.reply(threadId, req)
  } catch (e) {
    if (!mayHaveLanded(e)) throw e
    await lookForCreated(deps, e, before, req.body, { threadId })
    return
  }
  const { comment, pendingReview } = replied
  // A single reply lands in a pending review started meanwhile (on github.com, say); the server
  // reads which, but if that read failed the review isn't known: read it, don't drop it. Nor
  // when GitHub didn't send the reply back, so it can't tell where it went.
  const reviewKnown = pendingReview !== null || (comment !== null && comment.state !== 'PENDING')
  if (reviewKnown) deps.store.applyPendingReview(pendingReview)
  if (!comment || !deps.store.applyComment(threadId, comment) || !reviewKnown) deps.reread()
}

/**
 * Deletes a comment; resolves once the store no longer has it, with whether its thread went
 * with it (it was the last comment). When the outcome is unknown, a fresh read tells.
 */
export async function postDelete(deps: DeleteDeps, threadId: string, commentId: string): Promise<{ threadGone: boolean }> {
  let deleted: DeletedComment
  try {
    deleted = await deps.remove(threadId, commentId)
  } catch (e) {
    if (!mayHaveLanded(e)) throw e
    await deps.settle(0)
    const requestedAt = performance.now()
    let fresh: ReviewThreads
    try {
      fresh = await deps.read()
    } catch {
      throw new Error(`${errorMessage(e)}. It may have been deleted all the same: refresh to see`)
    }
    const thread = fresh.threads.find((t) => t.id === threadId)
    const kept = thread?.comments.some((c) => c.id === commentId) ?? false
    if (!kept) deps.gone(!thread)
    deps.store.rebase(fresh, requestedAt)
    if (kept) throw e
    // A write to the thread that settled meanwhile kept the store's version, with the comment.
    deps.store.dropComment(threadId, commentId, !thread)
    return { threadGone: !thread }
  }
  deps.gone(!deleted.thread)
  deps.store.applyPendingReview(deleted.pendingReview)
  deps.store.applyThread(threadId, deleted.thread)
  return { threadGone: !deleted.thread }
}

/** Reads taken before saying a comment wasn't posted. */
const LOOKS = 2

/** The comment a create whose outcome is unknown posted after all; throws when it isn't found. */
async function lookForCreated(deps: ReadDeps, cause: unknown, before: ReadonlySet<string>, body: string, where: CreatedWhere): Promise<ReviewComment> {
  for (let attempt = 0; ; attempt++) {
    await deps.settle(attempt)
    const requestedAt = performance.now()
    let fresh: ReviewThreads
    try {
      fresh = await deps.read()
    } catch {
      throw new Error(`${errorMessage(cause)}. It may have been posted all the same: refresh before sending it again`)
    }
    deps.store.rebase(fresh, requestedAt)
    const found = findCreated(fresh.threads, before, body, where)
    if (found) return found.comment
    if (attempt + 1 >= LOOKS) {
      throw new Error(`${errorMessage(cause)}. It wasn't found on GitHub, so it most likely wasn't posted: you can send it again`)
    }
  }
}
