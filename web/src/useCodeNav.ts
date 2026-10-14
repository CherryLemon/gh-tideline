// Looking up a name: Ctrl+click or Ctrl+B on a name asks the server for its definitions, building the index of the
// repository at that commit first when it has none (behind a dialog with progress that can be cancelled), and then goes
// to the one definition, or lists them to choose from. One look-up at a time: a new one, a move of the caret, or leaving
// the history entry ends the one under way.
import { useCallback, useEffect, useRef, useState } from 'react'
import { flushSync } from 'react-dom'
import {
  ApiError,
  cancelIndex,
  errorMessage,
  getDefinitions,
  getIndex,
  startIndex,
  type ChangedFile,
  type IndexStatus,
  type NavHit,
  type NavPos,
  type NavResult,
  type NavSide,
  type RepoRef,
} from './api'
import type { IndexProgress } from './components/IndexDialog'
import type { NavPopupProps } from './components/NavPopup'
import type { NavSource } from './monaco/links'
import { choose } from './nav/choose'
import { getSymbols } from './nav/symbols'
import { resolveTarget } from './nav/targets'
import { fileQuery } from './router'

/** A look-up that takes shorter than this to get its index shows no dialog: one that is built already is no wait. */
const GRACE_MS = 300
const POLL_MS = 300
/** Times a look-up builds the index again: a job that vanished (the server restarted) or an index that went missing. */
const MAX_BUILDS = 3

const UNSUPPORTED = "Go to definition doesn't support this file type"

export interface CodeNavDeps {
  repo: RepoRef
  prNumber: number
  /** The head the page shows. */
  head: string
  files: readonly ChangedFile[]
  /** The paths of the head tree, once it is listed. */
  tree: { has(path: string): boolean } | null
  /** Moves to a place as a new history entry. */
  goTo(url: string, to: NavPos): void
  showToast(text: string, action?: { label: string; run(): void }): void
  onRefresh(): void
  /** The history entry shown: a look-up ends when the user leaves it. */
  entryId: string
}

export interface CodeNav {
  /** Looks up the definitions of the name; replaces the look-up under way. */
  request(source: NavSource): void
  /** Ends the look-up under way and closes what it shows. */
  dismiss(): void
  /** Whether the look-up shows something: shortcuts of the page stay off. */
  open: boolean
  /** The dialog for the index being built; shown once a look-up has waited for it for a moment. */
  index: (IndexProgress & { onCancel(): void; onRetry(): void; onClose(): void }) | null
  /** The list to choose from; `id` is new for each. */
  list: { id: number; props: NavPopupProps } | null
}

/** One look-up. */
interface Run {
  abort: AbortController
}

const sleep = (ms: number, signal: AbortSignal) =>
  new Promise<void>((resolve) => {
    if (signal.aborted) return resolve()
    const done = () => {
      clearTimeout(timer)
      signal.removeEventListener('abort', done)
      resolve()
    }
    const timer = setTimeout(done, ms)
    signal.addEventListener('abort', done, { once: true })
  })

export function useCodeNav(deps: CodeNavDeps): CodeNav {
  const latest = useRef(deps)
  latest.current = deps
  const [index, setIndex] = useState<CodeNav['index']>(null)
  const [list, setList] = useState<CodeNav['list']>(null)
  const current = useRef<Run | null>(null)
  const listId = useRef(0)

  const dismiss = useCallback(() => {
    const run = current.current
    current.current = null
    run?.abort.abort()
    setIndex(null)
    setList(null)
  }, [])
  // The look-up belongs to the entry and the head it was made at; and ends with the page.
  useEffect(() => dismiss, [deps.entryId, deps.head, dismiss])

  /** Goes to a definition: the dialogs close first, so that the editor takes the focus once the place is shown. */
  const open = useCallback((source: NavSource, hit: NavHit) => {
    const { files, tree, goTo, showToast } = latest.current
    flushSync(() => {
      setIndex(null)
      setList(null)
    })
    current.current = null
    const target = resolveTarget(hit.path, source.click.side, files, tree)
    if (!target) return showToast(`${hit.path} isn't one of this pull request's files`)
    goTo(location.pathname + fileQuery(target.path, location.search), { path: target.path, side: target.side, line: hit.line, column: hit.column })
  }, [])

  /** Waits for the index of the commit of `side` to be built, starting its build if need be; false if the look-up is to stop. */
  const build = useCallback(async (run: Run, side: NavSide): Promise<boolean> => {
    const { repo, prNumber, head, showToast } = latest.current
    const { signal } = run.abort
    const started = performance.now()
    let shown = false
    const stop = () => {
      // The page goes on without it, and so does the build: it is the user's Cancel that stops that.
      dismiss()
      void cancelIndex(repo, prNumber, head, side).catch(() => {})
    }
    const show = (status: Pick<IndexStatus, 'phase' | 'done' | 'total'>, failure: string | null, onRetry: () => void, onClose: () => void) => {
      shown = true
      setIndex({ side, phase: status.phase, done: status.done, total: status.total, failure, onCancel: stop, onRetry, onClose })
    }
    let status = await startIndex(repo, prNumber, head, side)
    for (let builds = 1; !signal.aborted; ) {
      switch (status.state) {
        case 'ready':
          return true
        case 'failed': {
          // Until the user decides: the look-up is still theirs to go on with.
          const retry = await new Promise<boolean>((resolve) => {
            signal.addEventListener('abort', () => resolve(false), { once: true })
            show(status, status.message ?? 'Indexing failed', () => resolve(true), () => resolve(false))
          })
          if (!retry || signal.aborted) return false
          status = await startIndex(repo, prNumber, head, side)
          continue
        }
        case 'cancelled':
          // Not by this page, which would have stopped (another window did).
          showToast('Indexing was cancelled')
          return false
        case 'missing':
          // The job is gone.
          if (++builds > MAX_BUILDS) throw new Error('The index could not be built')
          status = await startIndex(repo, prNumber, head, side)
          continue
        default:
          if (shown || performance.now() - started >= GRACE_MS) show(status, null, () => {}, dismiss)
      }
      await sleep(POLL_MS, signal)
      if (signal.aborted) return false
      status = await getIndex(repo, prNumber, head, side, signal)
    }
    return false
  }, [dismiss])

  const lookUp = useCallback(
    async (run: Run, source: NavSource) => {
      const { repo, prNumber, head, showToast, onRefresh } = latest.current
      const { signal } = run.abort
      const { click } = source
      try {
        // The symbols of the file tell a click on a definition's own name from one on a use of the name.
        const file = source.oid === null ? null : await getSymbols(repo, source.oid, click.path).catch(() => null)
        if (signal.aborted) return
        if (file?.language === null) return showToast(UNSUPPORTED)
        const query = { head, side: click.side, path: click.path, name: click.name }
        let result: NavResult | null = null
        for (let builds = 0; result === null; builds++) {
          try {
            result = await getDefinitions(repo, prNumber, query, signal)
          } catch (e) {
            if (!(e instanceof ApiError && e.code === 'index_missing') || builds >= MAX_BUILDS) throw e
            if (!(await build(run, click.side))) return
            // The dialog is closed for the look-up that follows.
            setIndex(null)
          }
        }
        if (signal.aborted) return
        if (result.language === null) return showToast(UNSUPPORTED)
        const choice = choose(click, file?.symbols ?? [], result)
        switch (choice.kind) {
          case 'jump':
            return open(source, choice.hit)
          case 'popup':
            setList({
              id: ++listId.current,
              props: {
                title: `Choose declaration of ${click.name} (${choice.hits.length})`,
                hits: choice.hits,
                anchor: source.anchor,
                onPick: (hit) => open(source, hit),
                onClose: dismiss,
              },
            })
            return
          case 'none':
            return showToast(`No definition found for ${click.name}`)
          case 'usages':
            return showToast(`${click.name} is defined here`)
        }
      } catch (e) {
        if (signal.aborted) return
        if (e instanceof ApiError && e.code === 'stale_head') showToast(e.message, { label: 'Refresh', run: onRefresh })
        else showToast(errorMessage(e))
      } finally {
        if (current.current === run) {
          current.current = null
          setIndex(null)
        }
      }
    },
    [build, open, dismiss],
  )

  const request = useCallback(
    (source: NavSource) => {
      dismiss()
      const run: Run = { abort: new AbortController() }
      current.current = run
      void lookUp(run, source)
    },
    [dismiss, lookUp],
  )

  return { request, dismiss, open: index !== null || list !== null, index, list }
}
