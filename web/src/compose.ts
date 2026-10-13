// Posting a new review thread and taking GitHub's answer into the review store. Creates are
// never retried: when the outcome is unknown (the server or GitHub failed after the request
// went out), a fresh read tells whether the comment landed.
import type { CommentMode, CreatedThread, NewThreadRequest, ReviewThreads } from './api'
import type { DraftView } from './drafts'
import { errorMessage, mayHaveLanded } from './api'
import { findCreated, ownCommentIds, type CreatedWhere, type ReviewStore } from './review'

export interface PostDeps {
  store: ReviewStore
  create(req: NewThreadRequest): Promise<CreatedThread>
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
    return lookForCreated(deps, e, before, req)
  }
  deps.store.applyPendingReview(created.pendingReview)
  if (created.thread) deps.store.applyThread(created.thread.id, created.thread)
  else deps.reread()
  return created.submitError ? { submitError: created.submitError } : {}
}

/** Reads taken before saying a comment wasn't posted. */
const LOOKS = 2

async function lookForCreated(deps: PostDeps, cause: unknown, before: ReadonlySet<string>, req: NewThreadRequest): Promise<Posted> {
  const where: CreatedWhere =
    req.subjectType === 'FILE'
      ? { path: req.path, subjectType: 'FILE' }
      : { path: req.path, subjectType: 'LINE', side: req.side, line: req.line }
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
    const found = findCreated(fresh.threads, before, req.body, where)
    // A single comment whose review couldn't be submitted waits in a pending review.
    if (found) return req.mode === 'single' && found.comment.state === 'PENDING' ? { submitError: errorMessage(cause) } : {}
    if (attempt + 1 >= LOOKS) {
      throw new Error(`${errorMessage(cause)}. It wasn't found on GitHub, so it most likely wasn't posted: you can send it again`)
    }
  }
}
