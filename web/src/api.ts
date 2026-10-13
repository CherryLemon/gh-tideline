// HTTP client for the tideline backend. Types mirror the JSON the Rust server sends (design.md §6).

export interface PrSummary {
  host: string
  owner: string
  repo: string
  number: number
  title: string
  author: string
  state: 'OPEN' | 'CLOSED' | 'MERGED'
  url: string
  base: { ref: string; sha: string }
  head: { ref: string; sha: string }
  mergeBase: string
  nodeId: string
  /** Login of the user the server's GitHub token belongs to. */
  viewer: string
}

/** DISMISSED: viewed, then changed by a later push. */
export type ViewedState = 'VIEWED' | 'UNVIEWED' | 'DISMISSED'

export type FileStatus = 'A' | 'M' | 'D' | 'R' | 'C' | 'T'

export interface ChangedFile {
  path: string
  /** Only present for R / C. */
  oldPath?: string
  status: FileStatus
  /** null for added files. */
  oldOid: string | null
  /** null for deleted files. */
  newOid: string | null
  oldMode: string
  newMode: string
  additions?: number
  deletions?: number
  binary: boolean
  generated: boolean
  /** Only on the full PR file list, and only for paths GitHub lists. */
  viewed?: ViewedState
}

/** A file of the head tree (`git ls-tree -r`); gitlinks (mode 160000) carry a commit oid. */
export interface TreeEntry {
  path: string
  mode: string
  oid: string
}

export interface PrTree {
  /** The head commit the listing belongs to. */
  head: string
  /** Empty when `head` is not the head that was asked for (the PR has moved on). */
  entries: TreeEntry[]
}

/** LEFT is the old (base) file, RIGHT the new (head) file. */
export type DiffSide = 'LEFT' | 'RIGHT'

export interface ReviewAuthor {
  login: string
  avatarUrl: string | null
}

export interface ReviewComment {
  id: string
  /** null for a deleted account ("ghost"). */
  author: ReviewAuthor | null
  /** Markdown source. */
  body: string
  /** GitHub-rendered HTML; only ever displayed through `sanitize.ts`. */
  bodyHtml: string
  createdAt: string
  lastEditedAt: string | null
  url: string
  /** PENDING: part of the viewer's unsubmitted review. */
  state: 'PENDING' | 'SUBMITTED'
  reviewId: string | null
  replyTo: string | null
  isMinimized: boolean
  minimizedReason: string | null
  viewerCanUpdate: boolean
  viewerCanDelete: boolean
  viewerDidAuthor: boolean
}

export interface ReviewThread {
  id: string
  /** GitHub's path; a LEFT thread of a renamed file may carry the old path. */
  path: string
  subjectType: 'LINE' | 'FILE'
  /** null for file-level threads. */
  side: DiffSide | null
  /** Only for multi-line threads (it may differ from `side`). */
  startSide: DiffSide | null
  /** Line in the whole file on `side` at the threads' head; null when outdated or file-level. */
  line: number | null
  startLine: number | null
  originalLine: number | null
  originalStartLine: number | null
  /** The diff hunk GitHub showed when the thread was started (plain text). */
  diffHunk: string
  isResolved: boolean
  isOutdated: boolean
  isCollapsed: boolean
  resolvedBy: string | null
  viewerCanReply: boolean
  viewerCanResolve: boolean
  viewerCanUnresolve: boolean
  comments: ReviewComment[]
}

/** The viewer's own unsubmitted review. */
export interface PendingReview {
  id: string
  body: string
  /** The commit the review was started on; it stays there when the PR moves on. */
  commit: string | null
  createdAt: string
  url: string
}

export interface ReviewThreads {
  /** The head the line numbers refer to (compare it with the head the page shows). */
  head: string
  locked: boolean
  pendingReview: PendingReview | null
  threads: ReviewThread[]
}

/** `single`: post now ("Add single comment"); `review`: add to the viewer's pending review. */
export type CommentMode = 'single' | 'review'

/** What a new thread is on: the file, or a line or range of lines on one side. */
export type NewThreadTarget =
  | { subjectType: 'FILE' }
  | { subjectType: 'LINE'; side: DiffSide; line: number; startLine?: number; startSide?: DiffSide }

export type NewThreadRequest = NewThreadTarget & {
  /** The head the lines refer to. */
  head: string
  path: string
  body: string
  mode: CommentMode
}

export interface CreatedThread {
  /** null when the comment was posted but its thread couldn't be read back: read again. */
  thread: ReviewThread | null
  pendingReview: PendingReview | null
  /** A single comment on a file that was added but not submitted (it stays pending): why. */
  submitError?: string
}

export interface ReplyRequest {
  /** The head the page shows: a reply that starts a review starts it there. */
  head: string
  body: string
  mode: CommentMode
}

export interface Replied {
  /** null when the reply was posted but GitHub didn't return it: read again. */
  comment: ReviewComment | null
  pendingReview: PendingReview | null
}

export interface DeletedComment {
  /** null once the thread has no comments left (GitHub drops it). */
  thread: ReviewThread | null
  /** GitHub drops a pending review with its last comment. */
  pendingReview: PendingReview | null
}

export interface ApiErrorBody {
  error: { code: string; message: string }
}

export interface RepoRef {
  host: string
  owner: string
  repo: string
}

export const UNAUTHORIZED_MESSAGE = 'Run `gh-tideline` in a terminal to authorize this browser'
export const STALE_HEAD_MESSAGE = 'The pull request has been updated; refresh to load the new version'

const CODE_MESSAGES: Record<string, string> = {
  unauthorized: UNAUTHORIZED_MESSAGE,
  forbidden_host: 'Host not allowed',
  not_found: 'Not found',
  no_token: 'No GitHub token',
  github_error: 'GitHub request failed',
  git_error: 'Git operation failed',
  bad_request: 'Invalid request',
  stale_head: STALE_HEAD_MESSAGE,
  rejected: 'GitHub refused the change',
  pending_review_exists: 'You have a pending review: add the comment to it, or submit or discard the review first',
  review_changed: 'Your pending review changed on GitHub; refresh to see it',
  http_413: 'The comment is too long',
  network: "Can't reach the Tideline server",
}

/** Codes whose fixed text says it all (the server's detail would repeat it). */
const FIXED_MESSAGES = new Set(['unauthorized', 'stale_head', 'pending_review_exists', 'review_changed', 'http_413'])

export class ApiError extends Error {
  readonly status: number
  readonly code: string
  readonly detail: string

  constructor(status: number, code: string, detail: string) {
    const base = CODE_MESSAGES[code] ?? `Request failed (HTTP ${status})`
    super(FIXED_MESSAGES.has(code) || !detail || detail === base ? base : `${base}: ${detail}`)
    this.name = 'ApiError'
    this.status = status
    this.code = code
    this.detail = detail
  }
}

async function toApiError(res: Response): Promise<ApiError> {
  let code = res.status === 401 ? 'unauthorized' : `http_${res.status}`
  let detail = ''
  try {
    const body = (await res.json()) as Partial<ApiErrorBody>
    if (body.error) {
      code = body.error.code || code
      detail = body.error.message || ''
    }
  } catch {
    detail = res.statusText
  }
  if (res.status === 401) code = 'unauthorized'
  return new ApiError(res.status, code, detail)
}

async function request(path: string, init?: RequestInit): Promise<Response> {
  const headers = new Headers(init?.headers)
  headers.set('X-DR', '1')
  let res: Response
  try {
    res = await fetch(path, { ...init, headers, credentials: 'same-origin' })
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') throw e
    throw new ApiError(0, 'network', (e as Error).message)
  }
  if (!res.ok) throw await toApiError(res)
  return res
}

const seg = encodeURIComponent

function repoBase(r: RepoRef): string {
  return `/api/${seg(r.host)}/${seg(r.owner)}/${seg(r.repo)}`
}

export async function getPr(r: RepoRef, n: number, refresh = false): Promise<PrSummary> {
  const res = await request(`${repoBase(r)}/pulls/${n}${refresh ? '?refresh=1' : ''}`)
  return (await res.json()) as PrSummary
}

export async function getPrFiles(r: RepoRef, n: number, refresh = false): Promise<ChangedFile[]> {
  const res = await request(`${repoBase(r)}/pulls/${n}/files${refresh ? '?refresh=1' : ''}`)
  return (await res.json()) as ChangedFile[]
}

/**
 * Every file of the head tree, for the full project tree. The server lists `head` if it
 * still has that snapshot, else its current head (compare `PrTree.head`).
 */
export async function getPrTree(r: RepoRef, n: number, head: string, signal?: AbortSignal): Promise<PrTree> {
  const res = await request(`${repoBase(r)}/pulls/${n}/tree?head=${encodeURIComponent(head)}`, { signal })
  return (await res.json()) as PrTree
}

/** Marks (`viewed`) or unmarks a file as viewed; `head` is the head the page shows. */
export async function setViewed(r: RepoRef, n: number, path: string, viewed: boolean, head: string): Promise<ViewedState> {
  const res = await request(`${repoBase(r)}/pulls/${n}/viewed`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path, viewed, head }),
  })
  return ((await res.json()) as { viewed: ViewedState }).viewed
}

/** Review threads and the viewer's pending review, read fresh from GitHub. */
export async function getThreads(r: RepoRef, n: number, signal?: AbortSignal): Promise<ReviewThreads> {
  const res = await request(`${repoBase(r)}/pulls/${n}/threads`, { signal })
  return (await res.json()) as ReviewThreads
}

/** Resolves (`resolved`) or unresolves a thread; returns it as GitHub has it afterwards. */
export async function setThreadResolved(r: RepoRef, n: number, threadId: string, resolved: boolean): Promise<ReviewThread> {
  const res = await request(`${repoBase(r)}/pulls/${n}/threads/${seg(threadId)}/resolved`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ resolved }),
  })
  return (await written<{ thread: ReviewThread }>(res)).thread
}

/** The answer to a write; one cut short (after the server did the write) is like a lost connection. */
async function written<T>(res: Response): Promise<T> {
  try {
    return (await res.json()) as T
  } catch (e) {
    throw new ApiError(0, 'network', errorMessage(e))
  }
}

/** Starts a review thread (see `CommentMode`). Never retried: see `findCreated`. */
export async function createThread(r: RepoRef, n: number, req: NewThreadRequest): Promise<CreatedThread> {
  const res = await request(`${repoBase(r)}/pulls/${n}/threads`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(req),
  })
  return written<CreatedThread>(res)
}

/** Replies to a thread (see `CommentMode`). Never retried, like `createThread`. */
export async function replyToThread(r: RepoRef, n: number, threadId: string, req: ReplyRequest): Promise<Replied> {
  const res = await request(`${repoBase(r)}/pulls/${n}/threads/${seg(threadId)}/replies`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(req),
  })
  return written<Replied>(res)
}

/** Replaces the text of one of the viewer's comments; returns the comment as GitHub has it now. */
export async function editComment(r: RepoRef, n: number, threadId: string, commentId: string, body: string): Promise<ReviewComment> {
  const res = await request(`${repoBase(r)}/pulls/${n}/threads/${seg(threadId)}/comments/${seg(commentId)}`, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ body }),
  })
  return (await written<{ comment: ReviewComment }>(res)).comment
}

/** Deletes one of the viewer's comments (one already gone counts as deleted). */
export async function deleteComment(r: RepoRef, n: number, threadId: string, commentId: string): Promise<DeletedComment> {
  const res = await request(`${repoBase(r)}/pulls/${n}/threads/${seg(threadId)}/comments/${seg(commentId)}`, { method: 'DELETE' })
  return written<DeletedComment>(res)
}

/** Deletes the pending review `reviewId` (the one shown) with all its comments. */
export async function discardReview(r: RepoRef, n: number, reviewId: string): Promise<void> {
  await request(`${repoBase(r)}/pulls/${n}/review`, {
    method: 'DELETE',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ reviewId }),
  })
}

/**
 * Whether a failed write may still have landed on GitHub: the server couldn't be reached, or
 * GitHub failed or timed out (it may have done the write first). Look before trying again.
 */
export function mayHaveLanded(e: unknown): boolean {
  return e instanceof ApiError && (e.code === 'network' || e.status === 502 || e.status === 504)
}

/** Raw blob response; the caller decides whether to read the body (see Content-Length checks). */
export function getBlob(r: RepoRef, oid: string, signal?: AbortSignal): Promise<Response> {
  return request(`${repoBase(r)}/blobs/${oid}`, { signal })
}

export function errorMessage(e: unknown): string {
  if (e instanceof Error) return e.message
  return String(e)
}
