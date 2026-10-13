// Viewed marks: optimistic toggling with at most one request in flight per path.
import type { ChangedFile, ViewedState } from './api'

export type SendViewed = (path: string, viewed: boolean) => Promise<ViewedState>

/**
 * Holds the confirmed marks from the server plus the user's pending intents. The UI shows
 * the intent right away; per path only one request is in flight, and when it settles the
 * latest intent is sent if it still differs. A failed request drops the intent (rollback).
 * One instance lives as long as the page shows one head; refreshes go through `rebase`.
 */
export class ViewedSync {
  private readonly confirmed: Map<string, ViewedState>
  private readonly intent = new Map<string, boolean>()
  private readonly inflight = new Set<string>()
  /** When the last request for a path settled (`performance.now()`). */
  private readonly settled = new Map<string, number>()
  /** `requestedAt` of the newest list taken by `rebase`. */
  private rebasedAt = -Infinity
  private disposed = false

  constructor(
    initial: ReadonlyMap<string, ViewedState>,
    private readonly send: SendViewed,
    private readonly onChange: () => void,
    private readonly onError: (path: string, error: unknown) => void,
  ) {
    this.confirmed = new Map(initial)
  }

  /** Marks as displayed: pending intents over confirmed state. */
  snapshot(): Map<string, ViewedState> {
    const out = new Map(this.confirmed)
    for (const [path, viewed] of this.intent) out.set(path, viewed ? 'VIEWED' : 'UNVIEWED')
    return out
  }

  isViewed(path: string): boolean {
    const want = this.intent.get(path)
    return want ?? this.confirmed.get(path) === 'VIEWED'
  }

  /** Whether GitHub tracks a mark for `path` (its rename detection may differ from ours). */
  has(path: string): boolean {
    return this.confirmed.has(path)
  }

  set(path: string, viewed: boolean): void {
    if (this.disposed || !this.confirmed.has(path)) return
    this.intent.set(path, viewed)
    this.onChange()
    if (!this.inflight.has(path)) this.pump(path)
  }

  /**
   * Takes the marks of a file list requested at `requestedAt` (`performance.now()`), except
   * for paths with a request in flight or pending, or one that settled after the request
   * was sent: the server may have read those before our change reached GitHub. A list
   * requested before one already taken is ignored. Paths no longer listed are dropped,
   * with their pending intent.
   */
  rebase(marks: ReadonlyMap<string, ViewedState>, requestedAt: number): void {
    if (this.disposed || requestedAt < this.rebasedAt) return
    this.rebasedAt = requestedAt
    for (const path of [...this.confirmed.keys()]) {
      if (marks.has(path)) continue
      this.confirmed.delete(path)
      this.intent.delete(path)
    }
    for (const [path, state] of marks) {
      const busy = this.inflight.has(path) || this.intent.has(path)
      if (busy || (this.settled.get(path) ?? -Infinity) >= requestedAt) continue
      this.confirmed.set(path, state)
    }
    this.onChange()
  }

  /** Stops notifying and sending; a request in flight completes, nothing follows it. */
  dispose(): void {
    this.disposed = true
  }

  private pump(path: string): void {
    if (this.disposed) return
    const want = this.intent.get(path)
    if (want === undefined) return
    if ((this.confirmed.get(path) === 'VIEWED') === want) {
      this.intent.delete(path)
      this.onChange()
      return
    }
    this.inflight.add(path)
    this.send(path, want).then(
      (state) => {
        this.inflight.delete(path)
        this.settled.set(path, performance.now())
        // Unless a refresh dropped the path meanwhile.
        if (this.confirmed.has(path)) this.confirmed.set(path, state)
        this.pump(path)
      },
      (error: unknown) => {
        this.inflight.delete(path)
        this.settled.set(path, performance.now())
        this.intent.delete(path)
        if (this.disposed) return
        this.onChange()
        this.onError(path, error)
      },
    )
  }
}

/** Initial marks from the full PR file list. */
export function viewedMap(files: readonly ChangedFile[]): Map<string, ViewedState> {
  const m = new Map<string, ViewedState>()
  for (const f of files) if (f.viewed) m.set(f.path, f.viewed)
  return m
}

/**
 * Index of the next file after `current` (in tree order, wrapping around) that has a mark
 * and is not VIEWED, preferring files that are not generated; null if there is none.
 */
export function nextUnviewed(
  order: readonly { path: string; generated?: boolean }[],
  current: number,
  marks: ReadonlyMap<string, ViewedState>,
): number | null {
  let generated: number | null = null
  for (let step = 1; step < order.length; step++) {
    const i = (current + step + order.length) % order.length
    const mark = marks.get(order[i].path)
    if (mark === undefined || mark === 'VIEWED') continue
    if (!order[i].generated) return i
    generated ??= i
  }
  return generated
}

const NON_TEXT_INPUTS = new Set(['checkbox', 'radio', 'button', 'submit', 'reset', 'range', 'color', 'file', 'image'])

/**
 * Whether a key event target is a text field, so single-key shortcuts must not fire. The
 * Monaco editor's own input (a textarea or an EditContext element) does not count, but its
 * find/replace boxes (inside `.monaco-inputbox`) and the fields of review cards in it
 * (`.tl-zone`) do.
 */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false
  if (target.closest('.monaco-editor') && !target.closest('.monaco-inputbox, .tl-zone')) return false
  if (target instanceof HTMLElement && target.isContentEditable) return true
  if (target instanceof HTMLInputElement) return !NON_TEXT_INPUTS.has(target.type)
  return target.matches('textarea, select')
}
