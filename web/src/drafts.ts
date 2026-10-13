// Open comment boxes and what is typed in them, per pull request. Kept in sessionStorage, so a
// reload or a file switch doesn't lose a draft (and they go with the tab). Whether a box is
// sending, and why its last send failed, live here too: a box shown again shows them.
import type { DiffSide, ReviewThread } from './api'
import { errorMessage } from './api'

/**
 * What a box writes: a new thread on a file or on lines, a reply, an edit, the review summary.
 * A reply or an edit keeps its thread's path: should the thread go, the box shows there.
 */
export type DraftTarget =
  | { kind: 'file'; path: string }
  | { kind: 'lines'; path: string; side: DiffSide; startLine: number | null; line: number }
  | { kind: 'reply'; threadId: string; path: string }
  | { kind: 'edit'; threadId: string; commentId: string; path: string }
  | { kind: 'review' }

/** One box per target; the path comes last, so it may hold any character. */
export function draftKey(t: DraftTarget): string {
  switch (t.kind) {
    case 'file':
      return `file:${t.path}`
    case 'lines':
      return `lines:${t.side}:${t.startLine ?? ''}-${t.line}:${t.path}`
    case 'reply':
      return replyKey(t.threadId)
    case 'edit':
      return editKey(t.commentId)
    case 'review':
      return 'review'
  }
}

/** A comment's text as a box holds it: comments written on github.com keep CRLF, a textarea gives LF. */
export function boxText(body: string): string {
  return body.replace(/\r\n?/g, '\n')
}

/** The key of the box that replies to the thread. */
export function replyKey(threadId: string): string {
  return `reply:${threadId}`
}

/** The key of the box that edits the comment. */
export function editKey(commentId: string): string {
  return `edit:${commentId}`
}

export interface DraftView {
  target: DraftTarget
  /** The head the page showed when the box was opened (its line numbers refer to it). */
  head: string
  sending: boolean
  /** Why the last send failed; cleared by the next one. */
  error: string | null
}

export type DraftsSnapshot = ReadonlyMap<string, DraftView>

interface Stored {
  target: DraftTarget
  text: string
  head: string
  updatedAt: number
  /** Set while it is being sent: a reload meanwhile can't tell whether it was posted. */
  sentAt?: number
}

export type DraftStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>

/** Drafts not touched for this long are dropped when the page loads. */
const MAX_AGE_MS = 14 * 24 * 3600 * 1000

export const INTERRUPTED =
  'The page was reloaded while this was being sent, so it may have been posted: check before sending it again'

const stores = new Map<string, DraftStore>()

/**
 * The page's store for `scope`: one per pull request for the page's life, so a send on its way
 * still reports to the store a page shown again uses, and two stores never write one key.
 */
export function draftStoreFor(scope: string, storage: () => DraftStorage | null = sessionDraftStorage): DraftStore {
  let s = stores.get(scope)
  if (!s) {
    s = new DraftStore(scope, storage())
    stores.set(scope, s)
  }
  return s
}

/** sessionStorage, or null where it can't be used (then drafts last as long as the page). */
export function sessionDraftStorage(): DraftStorage | null {
  try {
    return window.sessionStorage
  } catch {
    return null
  }
}

function isTarget(t: unknown): t is DraftTarget {
  if (!t || typeof t !== 'object') return false
  const o = t as Record<string, unknown>
  switch (o.kind) {
    case 'file':
      return typeof o.path === 'string'
    case 'lines':
      return (
        typeof o.path === 'string' &&
        (o.side === 'LEFT' || o.side === 'RIGHT') &&
        typeof o.line === 'number' &&
        (o.startLine === null || typeof o.startLine === 'number')
      )
    case 'reply':
      return typeof o.threadId === 'string' && typeof o.path === 'string'
    case 'edit':
      return typeof o.threadId === 'string' && typeof o.commentId === 'string' && typeof o.path === 'string'
    case 'review':
      return true
    default:
      return false
  }
}

/**
 * The drafts of one pull request (`scope`). Read with `useSyncExternalStore`: the snapshot
 * changes when a box opens, closes, starts or stops sending, not on each keystroke (a box
 * keeps its text in its own state and reports it with `setText`).
 */
export class DraftStore {
  private readonly drafts = new Map<string, Stored>()
  private readonly sending = new Set<string>()
  private readonly errors = new Map<string, string>()
  private readonly listeners = new Set<() => void>()
  private snap: DraftsSnapshot | null = null
  private focusKey: string | null = null
  private readonly storageKey: string

  constructor(
    scope: string,
    private readonly storage: DraftStorage | null,
    now = Date.now(),
  ) {
    this.storageKey = `tl.drafts:${scope}`
    this.load(now)
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  getSnapshot = (): DraftsSnapshot => {
    if (!this.snap) {
      const views = new Map<string, DraftView>()
      for (const [key, d] of this.drafts) {
        views.set(key, { target: d.target, head: d.head, sending: this.sending.has(key), error: this.errors.get(key) ?? null })
      }
      this.snap = views
    }
    return this.snap
  }

  /**
   * Opens a box for `target` starting with `text` (or keeps the open one, with its text) and
   * asks it to take focus.
   */
  open(target: DraftTarget, head: string, text = '', now = Date.now()): string {
    const key = draftKey(target)
    if (!this.drafts.has(key)) {
      this.drafts.set(key, { target, text, head, updatedAt: now })
      this.save()
    }
    this.focusKey = key
    this.changed()
    return key
  }

  text(key: string): string {
    return this.drafts.get(key)?.text ?? ''
  }

  /** What the box shows now; saved, without a new snapshot. */
  setText(key: string, text: string, now = Date.now()): void {
    const d = this.drafts.get(key)
    if (!d || d.text === text) return
    d.text = text
    d.updatedAt = now
    this.save()
  }

  close(key: string): void {
    if (!this.drafts.delete(key)) return
    // A send still on its way finishes for nothing: a box opened again on the target is new.
    this.sending.delete(key)
    this.errors.delete(key)
    if (this.focusKey === key) this.focusKey = null
    this.save()
    this.changed()
  }

  /**
   * Sends the draft with `post`; the box closes once it resolves. A failure keeps the box with
   * the error. Nothing happens while the draft is being sent already. Whether it was sent.
   */
  async send(key: string, post: (text: string, draft: DraftView) => Promise<void>, now = Date.now()): Promise<boolean> {
    const d = this.drafts.get(key)
    if (!d || this.sending.has(key)) return false
    this.sending.add(key)
    this.errors.delete(key)
    d.sentAt = now
    this.save()
    this.changed()
    let failure: unknown = null
    let failed = false
    try {
      await post(d.text, { target: d.target, head: d.head, sending: true, error: null })
    } catch (e) {
      failure = e
      failed = true
    }
    // Closed meanwhile (and maybe opened again): that box isn't this one.
    if (this.drafts.get(key) !== d) return !failed
    this.sending.delete(key)
    if (failed) {
      delete d.sentAt
      this.errors.set(key, errorMessage(failure))
    } else {
      this.drafts.delete(key)
      this.errors.delete(key)
    }
    this.save()
    this.changed()
    return !failed
  }

  /** Moves the draft to `head` (the user checked its lines there). */
  setHead(key: string, head: string, now = Date.now()): void {
    const d = this.drafts.get(key)
    if (!d || d.head === head || this.sending.has(key)) return
    d.head = head
    d.updatedAt = now
    this.save()
    this.changed()
  }

  /** Whether the box `key` should take focus now (once per `open`). */
  takeFocus(key: string): boolean {
    if (this.focusKey !== key) return false
    this.focusKey = null
    return true
  }

  private changed(): void {
    this.snap = null
    for (const l of [...this.listeners]) l()
  }

  private load(now: number): void {
    let raw: string | null = null
    try {
      raw = this.storage?.getItem(this.storageKey) ?? null
    } catch {
      return
    }
    if (raw === null) return
    let parsed: unknown
    try {
      parsed = JSON.parse(raw)
    } catch {
      return
    }
    if ((parsed as { v?: unknown } | null)?.v !== 1) return
    const entries = (parsed as { drafts?: unknown }).drafts
    if (!entries || typeof entries !== 'object') return
    for (const [key, value] of Object.entries(entries as Record<string, unknown>)) {
      const d = value as Partial<Stored> | null
      if (!d || !isTarget(d.target) || draftKey(d.target) !== key) continue
      if (typeof d.text !== 'string' || typeof d.head !== 'string' || typeof d.updatedAt !== 'number') continue
      if (now - d.updatedAt > MAX_AGE_MS) continue
      this.drafts.set(key, { target: d.target, text: d.text, head: d.head, updatedAt: d.updatedAt })
      if (typeof d.sentAt === 'number') this.errors.set(key, INTERRUPTED)
    }
  }

  private save(): void {
    if (!this.storage) return
    try {
      if (this.drafts.size === 0) this.storage.removeItem(this.storageKey)
      else this.storage.setItem(this.storageKey, JSON.stringify({ v: 1, drafts: Object.fromEntries(this.drafts) }))
    } catch {
      // Full or unavailable: the drafts still live as long as the page.
    }
  }
}

/**
 * Keys of the reply and edit boxes whose thread (or comment) `threads` no longer has: deleted
 * on GitHub, or a discarded pending review took it. A box being sent isn't listed (its answer
 * says what happened).
 */
export function lostDrafts(views: DraftsSnapshot, threads: readonly ReviewThread[]): string[] {
  const out: string[] = []
  for (const [key, d] of views) {
    const t = d.target
    if (d.sending || (t.kind !== 'reply' && t.kind !== 'edit')) continue
    const thread = threads.find((th) => th.id === t.threadId)
    if (!thread || (t.kind === 'edit' && !thread.comments.some((c) => c.id === t.commentId))) out.push(key)
  }
  return out
}
