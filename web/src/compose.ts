// Posting a new review thread and taking GitHub's answer into the review store. Creates are
// never retried: when the outcome is unknown (the server or GitHub failed after the request
// went out), a fresh read tells whether the comment landed.
import type { CreatedThread, NewThreadRequest, ReviewThreads } from './api'
import { errorMessage, mayHaveLanded } from './api'
import { findCreated, ownCommentIds, type ReviewStore } from './review'

export interface PostDeps {
  store: ReviewStore
  create(req: NewThreadRequest): Promise<CreatedThread>
  /** A fresh read of the threads. */
  read(): Promise<ReviewThreads>
  /** Reads again through the page (which reports a failure itself). */
  reread(): void
  /** Before looking for a comment whose create failed: GitHub may still be storing it. */
  settle(): Promise<void>
}

export interface Posted {
  /** The comment was added, but the review that should have posted it is still pending. */
  submitError?: string
}

/** Posts `req`; resolves once the store has the new thread (or a read is on its way). */
export async function postThread(deps: PostDeps, req: NewThreadRequest): Promise<Posted> {
  const before = ownCommentIds(deps.store.getSnapshot().threads)
  let created: CreatedThread
  try {
    created = await deps.create(req)
  } catch (e) {
    if (!mayHaveLanded(e)) throw e
    await lookForCreated(deps, e, before, req)
    return {}
  }
  deps.store.applyPendingReview(created.pendingReview)
  if (created.thread) deps.store.applyThread(created.thread.id, created.thread)
  else deps.reread()
  return created.submitError ? { submitError: created.submitError } : {}
}

async function lookForCreated(deps: PostDeps, cause: unknown, before: ReadonlySet<string>, req: NewThreadRequest): Promise<void> {
  await deps.settle()
  const requestedAt = performance.now()
  let fresh: ReviewThreads
  try {
    fresh = await deps.read()
  } catch {
    throw new Error(`${errorMessage(cause)}. It may have been posted all the same: refresh before sending it again`)
  }
  deps.store.rebase(fresh, requestedAt)
  if (findCreated(fresh.threads, before, req.body, { path: req.path })) return
  throw new Error(`${errorMessage(cause)}. It isn't on GitHub, so you can send it again`)
}
