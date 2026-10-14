import { useSyncExternalStore } from 'react'
import type { NavPos } from './api'
import { newEntryId, pushedEntry, readEntry, replacedEntry, type NavEntryState } from './nav/history'

const NAV_EVENT = 'dr:navigate'

/**
 * The history entry shown. An entry the app didn't make (the page's first, or one from an older version)
 * gets its state here, so every entry the app looks at has an id and a depth.
 */
export function currentEntry(): NavEntryState {
  const entry = readEntry(history.state)
  if (entry) return entry
  const first = { id: newEntryId(), depth: 0 }
  history.replaceState(first, '')
  return first
}

/**
 * Change the URL (pushState by default) and notify subscribers. A push is a new entry, even to the same URL;
 * `pos` is the place it lands on, which the page reveals. A replace keeps the entry's id.
 */
export function navigate(url: string, replace = false, pos?: NavPos): void {
  const entry = currentEntry()
  if (replace) history.replaceState(replacedEntry(entry, pos), '', url)
  else history.pushState(pushedEntry(entry, newEntryId(), pos), '', url)
  window.dispatchEvent(new Event(NAV_EVENT))
}

/** Records where the caret is in the entry shown, for when the user comes back to it. Same URL and id, nobody is notified. */
export function rememberPos(pos: NavPos): void {
  history.replaceState(replacedEntry(currentEntry(), pos), '')
}

function subscribe(cb: () => void): () => void {
  window.addEventListener('popstate', cb)
  window.addEventListener(NAV_EVENT, cb)
  return () => {
    window.removeEventListener('popstate', cb)
    window.removeEventListener(NAV_EVENT, cb)
  }
}

function snapshot(): string {
  return location.pathname + location.search
}

const entryIdSnapshot = (): string => currentEntry().id

/**
 * Current `pathname + search` and the id of the history entry, re-rendering on pushState/replaceState via
 * navigate() and on popstate. The id changes with every push, so a push to the same URL re-renders too.
 */
export function useLocation(): { pathname: string; search: string; entryId: string } {
  const href = useSyncExternalStore(subscribe, snapshot)
  const entryId = useSyncExternalStore(subscribe, entryIdSnapshot)
  const q = href.indexOf('?')
  return q < 0 ? { pathname: href, search: '', entryId } : { pathname: href.slice(0, q), search: href.slice(q), entryId }
}

/**
 * `?file=<path>` (slashes kept readable) followed by every other parameter of `search`,
 * so switching files keeps the commit range (`from`/`to`/`range`).
 */
export function fileQuery(path: string, search = ''): string {
  const rest = new URLSearchParams(search)
  rest.delete('file')
  const tail = rest.toString()
  return `?file=${encodeURIComponent(path).replace(/%2F/g, '/')}${tail ? `&${tail}` : ''}`
}

export interface PrRoute {
  host: string
  owner: string
  repo: string
  number: number
}

/** Matches `/{host}/{owner}/{repo}/pull/{n}` (trailing segments such as `/files` are ignored). */
export function parsePrPath(pathname: string): PrRoute | null {
  const m = /^\/([^/]+)\/([^/]+)\/([^/]+)\/pull\/(\d+)(?:\/.*)?$/.exec(pathname)
  if (!m) return null
  return {
    host: decodeURIComponent(m[1]),
    owner: decodeURIComponent(m[2]),
    repo: decodeURIComponent(m[3]),
    number: Number(m[4]),
  }
}

/** Parse what a user pastes on the home page: a PR URL (github.com or GHE) or `owner/repo#n`. */
export function parsePrInput(input: string): PrRoute | null {
  const s = input.trim()
  const short = /^([\w.-]+)\/([\w.-]+)#(\d+)$/.exec(s)
  if (short) return { host: 'github.com', owner: short[1], repo: short[2], number: Number(short[3]) }
  const url = /^(?:https?:\/\/)?([^/\s]+)\/([^/\s]+)\/([^/\s]+)\/pull\/(\d+)(?:[/?#].*)?$/.exec(s)
  if (url) return { host: url[1].toLowerCase(), owner: url[2], repo: url[3], number: Number(url[4]) }
  return null
}

export function prPath(r: PrRoute): string {
  return `/${r.host}/${r.owner}/${r.repo}/pull/${r.number}`
}
