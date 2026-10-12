import { useSyncExternalStore } from 'react'

const NAV_EVENT = 'dr:navigate'

/** Change the URL (pushState by default) and notify subscribers. */
export function navigate(url: string, replace = false): void {
  if (replace) history.replaceState(null, '', url)
  else history.pushState(null, '', url)
  window.dispatchEvent(new Event(NAV_EVENT))
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

/** Current `pathname + search`, re-rendering on pushState/replaceState via navigate() and on popstate. */
export function useLocation(): { pathname: string; search: string } {
  const href = useSyncExternalStore(subscribe, snapshot)
  const q = href.indexOf('?')
  return q < 0 ? { pathname: href, search: '' } : { pathname: href.slice(0, q), search: href.slice(q) }
}

/** Encode a repository path for `?file=`, keeping slashes readable. */
export function fileQuery(path: string): string {
  return `?file=${encodeURIComponent(path).replace(/%2F/g, '/')}`
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
