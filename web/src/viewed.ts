// Viewed marks: optimistic toggling with at most one request in flight per path.
import type { ChangedFile, ViewedState } from './api'

export type SendViewed = (path: string, viewed: boolean) => Promise<ViewedState>

/**
 * Holds the confirmed marks from the server plus the user's pending intents. The UI shows
 * the intent right away; per path only one request is in flight, and when it settles the
 * latest intent is sent if it still differs. A failed request drops the intent (rollback).
 */
export class ViewedSync {
  private readonly confirmed: Map<string, ViewedState>
  private readonly intent = new Map<string, boolean>()
  private readonly inflight = new Set<string>()
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

  /** Stops notifying; requests still in flight complete silently. */
  dispose(): void {
    this.disposed = true
  }

  private pump(path: string): void {
    const want = this.intent.get(path)
    if (want === undefined) return
    if ((this.confirmed.get(path) === 'VIEWED') === want) {
      this.intent.delete(path)
      if (!this.disposed) this.onChange()
      return
    }
    this.inflight.add(path)
    this.send(path, want).then(
      (state) => {
        this.inflight.delete(path)
        this.confirmed.set(path, state)
        this.pump(path)
      },
      (error: unknown) => {
        this.inflight.delete(path)
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
 * and is not VIEWED; null if there is none.
 */
export function nextUnviewed(
  order: readonly { path: string }[],
  current: number,
  marks: ReadonlyMap<string, ViewedState>,
): number | null {
  for (let step = 1; step < order.length; step++) {
    const i = (current + step + order.length) % order.length
    const mark = marks.get(order[i].path)
    if (mark !== undefined && mark !== 'VIEWED') return i
  }
  return null
}

const NON_TEXT_INPUTS = new Set(['checkbox', 'radio', 'button', 'submit', 'reset', 'range', 'color', 'file', 'image'])

/**
 * Whether a key event target is a text field, so single-key shortcuts must not fire. The
 * Monaco editor's own input (a textarea or an EditContext element) does not count, but its
 * find/replace boxes (inside `.monaco-inputbox`) do.
 */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false
  if (target.closest('.monaco-editor') && !target.closest('.monaco-inputbox')) return false
  if (target instanceof HTMLElement && target.isContentEditable) return true
  if (target instanceof HTMLInputElement) return !NON_TEXT_INPUTS.has(target.type)
  return target.matches('textarea, select')
}
