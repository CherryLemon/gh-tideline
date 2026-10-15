// What the app keeps in `history.state`, so that Back and Forward return to a place and not just to a file.
// The router reads and writes it through these functions; they hold no state of their own.
import type { NavPos } from '../api'

export interface NavEntryState {
  /** Tells entries apart, also those with the same URL (a jump within one file). */
  id: string
  /** Entries pushed since the app first saw this tab's history: Back stays inside the app while it is above 0. */
  depth: number
  /** Where the caret was when the user left the entry (or rested in it), or the place a jump to it lands on. */
  pos?: NavPos
}

const isInt = (n: unknown, min: number): n is number => typeof n === 'number' && Number.isInteger(n) && n >= min

function readPos(v: unknown): NavPos | null {
  if (typeof v !== 'object' || v === null) return null
  const { path, side, line, column } = v as Record<string, unknown>
  if (typeof path !== 'string' || path === '' || (side !== 'head' && side !== 'base') || !isInt(line, 1) || !isInt(column, 1)) return null
  return { path, side, line, column }
}

/**
 * The entry state stored in `history.state`, or null when it isn't one (an entry the app didn't make, or
 * an older version's). A malformed place is dropped: the entry itself is still good for Back.
 */
export function readEntry(state: unknown): NavEntryState | null {
  if (typeof state !== 'object' || state === null) return null
  const { id, depth, pos } = state as Record<string, unknown>
  if (typeof id !== 'string' || id === '' || !isInt(depth, 0)) return null
  const place = readPos(pos)
  return place ? { id, depth, pos: place } : { id, depth }
}

/** The state of a new entry pushed after `current`, one level deeper; `pos` is the place it lands on. */
export function pushedEntry(current: NavEntryState, id: string, pos?: NavPos): NavEntryState {
  return pos ? { id, depth: current.depth + 1, pos } : { id, depth: current.depth + 1 }
}

/** The state of the entry that replaces `current` in place (same id and depth): with `pos` if given, else with the place it had. */
export function replacedEntry(current: NavEntryState, pos?: NavPos): NavEntryState {
  return pos ? { ...current, pos } : current
}

/** A new entry id. Random rather than a counter: a reload starts over, and the entries before it keep theirs. */
export function newEntryId(): string {
  return Array.from(crypto.getRandomValues(new Uint8Array(8)), (b) => b.toString(16).padStart(2, '0')).join('')
}
