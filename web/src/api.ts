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
  network: "Can't reach the Tideline server",
}

export class ApiError extends Error {
  readonly status: number
  readonly code: string
  readonly detail: string

  constructor(status: number, code: string, detail: string) {
    const base = CODE_MESSAGES[code] ?? `Request failed (HTTP ${status})`
    // These always show the fixed text; other codes append the server detail.
    const fixed = code === 'unauthorized' || code === 'stale_head'
    super(fixed || !detail || detail === base ? base : `${base}: ${detail}`)
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

/** Raw blob response; the caller decides whether to read the body (see Content-Length checks). */
export function getBlob(r: RepoRef, oid: string, signal?: AbortSignal): Promise<Response> {
  return request(`${repoBase(r)}/blobs/${oid}`, { signal })
}

export function errorMessage(e: unknown): string {
  if (e instanceof Error) return e.message
  return String(e)
}
