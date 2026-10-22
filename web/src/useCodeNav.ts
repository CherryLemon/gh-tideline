// Looking up a name: Ctrl+click or Ctrl+B on a name asks the server for its definitions, building the index of the
// repository at that commit first when it has none (behind a dialog with progress that can be cancelled), and then goes
// to the one definition, or lists them to choose from. On the name of a definition itself it lists the places that use
// the name instead. One look-up at a time: a new one, a move of the caret, going to a text field or a dialog, or leaving
// the history entry ends the one under way, and what it has not found yet is not shown.
import { useCallback, useEffect, useRef, useState } from 'react'
import { flushSync } from 'react-dom'
import {
  ApiError,
  cancelIndex,
  errorMessage,
  getDefinitions,
  getIndex,
  getReferences,
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
import { createBuilds, withIndex } from './nav/build'
import { choose } from './nav/choose'
import { getSymbols } from './nav/symbols'
import { resolveTarget } from './nav/targets'
import { declarationsText, usagesText } from './nav/usages'
import { fileQuery } from './router'
import { isTypingTarget } from './viewed'

const UNSUPPORTED = "Go to definition doesn't support this file type"

/** The builds of indexes this page has asked the server to stop, which the next look-up of the same commit waits for. */
const builds = createBuilds()

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
  /** Whether a look-up is waiting for the server, with no dialog of its own to say so. */
  looking: boolean
  /** The look-up that is under way, or was: new for each, which `looking` can't tell when a look-up replaces another (it stays on). */
  lookId: number
  /** The dialog for the index being built; shown once a look-up has waited for it for a moment. */
  index: (IndexProgress & { onCancel(): void; onRetry(): void; onClose(): void }) | null
  /** The list to choose from; `id` is new for each. */
  list: { id: number; props: NavPopupProps } | null
}

/** One look-up. */
interface Run {
  abort: AbortController
}

/** Whether the user is at a text field or in a dialog of the page, other than the ones a look-up shows: gone to write, or to something else. */
function elsewhere(): boolean {
  const active = document.activeElement
  return isTypingTarget(active) || !!active?.closest('[role="dialog"]:not(.index-dialog, .nav-popup)')
}

export function useCodeNav(deps: CodeNavDeps): CodeNav {
  const latest = useRef(deps)
  latest.current = deps
  const [index, setIndex] = useState<CodeNav['index']>(null)
  const [list, setList] = useState<CodeNav['list']>(null)
  const [looking, setLooking] = useState(false)
  const [lookId, setLookId] = useState(0)
  const current = useRef<Run | null>(null)
  const listId = useRef(0)

  const dismiss = useCallback(() => {
    const run = current.current
    current.current = null
    run?.abort.abort()
    setIndex(null)
    setList(null)
    setLooking(false)
  }, [])
  // The look-up belongs to the entry and the head it was made at; and ends with the page.
  useEffect(() => dismiss, [deps.entryId, deps.head, dismiss])
  // Whoever goes to write something (a comment, the finder's search) has moved on: the answer would take the focus from them.
  useEffect(() => {
    const onFocusIn = (e: FocusEvent) => {
      if (current.current && isTypingTarget(e.target)) dismiss()
    }
    document.addEventListener('focusin', onFocusIn, true)
    return () => document.removeEventListener('focusin', onFocusIn, true)
  }, [dismiss])

  /** Goes to a definition: the dialogs close first, so that the editor takes the focus once the place is shown. */
  const open = useCallback((source: NavSource, hit: NavHit) => {
    const { files, tree, goTo, showToast } = latest.current
    flushSync(() => {
      setIndex(null)
      setList(null)
      setLooking(false)
    })
    current.current = null
    const target = resolveTarget(hit.path, source.click.side, files, tree)
    if (!target) return showToast(`${hit.path} isn't one of this pull request's files`)
    goTo(location.pathname + fileQuery(target.path, location.search), { path: target.path, side: target.side, line: hit.line, column: hit.column })
  }, [])

  /** Waits for the index of the commit of `side` to be built, starting its build if need be; false if the look-up is to stop. */
  const build = useCallback(
    (run: Run, side: NavSide): Promise<boolean> => {
      const { repo, prNumber, head, showToast } = latest.current
      const key = `${repo.host}/${repo.owner}/${repo.repo}#${prNumber}@${head}:${side}`
      const stop = () => {
        // The page goes on without it, and so does the build: it is the user's Cancel that stops that.
        dismiss()
        builds.stop(key, () => cancelIndex(repo, prNumber, head, side))
      }
      const show = (status: Pick<IndexStatus, 'phase' | 'done' | 'total'>, failure: string | null, onRetry: () => void, onClose: () => void) =>
        setIndex({ side, phase: status.phase, done: status.done, total: status.total, failure, onCancel: stop, onRetry, onClose })
      return builds.wait(
        key,
        { start: () => startIndex(repo, prNumber, head, side), poll: (signal) => getIndex(repo, prNumber, head, side, signal) },
        {
          progress: (status) => show(status, null, () => {}, dismiss),
          failed: (message, retry, close) => show({ phase: null, done: 0, total: 0 }, message, retry, close),
          notify: showToast,
        },
        run.abort.signal,
      )
    },
    [dismiss],
  )

  /** What `get` asks the server, once the index of the commit of `side` is there to answer: built first if it is not; null if the look-up is to stop. */
  const indexed = useCallback(
    (run: Run, side: NavSide, get: () => Promise<NavResult>): Promise<NavResult | null> =>
      // The dialog is closed for the look-up that follows.
      withIndex(get, () => build(run, side), () => setIndex(null)),
    [build],
  )

  const lookUp = useCallback(
    async (run: Run, source: NavSource) => {
      const { repo, prNumber, head, showToast, onRefresh } = latest.current
      const { signal } = run.abort
      const { click } = source
      // Whether what comes in is still wanted: not after the look-up was ended, nor with the user at a text field or a dialog.
      const stale = () => signal.aborted || elsewhere()
      try {
        // The symbols of the file tell a click on a definition's own name from one on a use of the name.
        const file = source.oid === null ? null : await getSymbols(repo, source.oid, click.path).catch(() => null)
        if (stale()) return
        if (file?.language === null) return showToast(UNSUPPORTED)
        const query = { head, side: click.side, path: click.path, name: click.name }
        const result = await indexed(run, click.side, () => getDefinitions(repo, prNumber, query, signal))
        if (!result || stale()) return
        if (result.language === null) return showToast(UNSUPPORTED)
        const choice = choose(click, file?.symbols ?? [], result)
        switch (choice.kind) {
          case 'jump':
            return open(source, choice.hit)
          case 'popup':
            setList({
              id: ++listId.current,
              props: { ...declarationsText(click.name, result), hits: choice.hits, anchor: source.anchor, onPick: (hit) => open(source, hit), onClose: dismiss },
            })
            return
          case 'none':
            return showToast(`No definition found for ${click.name}`)
          case 'usages': {
            // A definition is no place to go to from its own name: the answer is where the name is used. By the name the definition has,
            // which the word clicked may be a part of (`operator` of `operator==`).
            const named = { ...query, name: choice.name }
            const usages = await indexed(run, click.side, () => getReferences(repo, prNumber, named, signal))
            if (!usages || stale()) return
            setList({
              id: ++listId.current,
              props: { ...usagesText(choice.name, usages), hits: usages.hits, anchor: source.anchor, onPick: (hit) => open(source, hit), onClose: dismiss },
            })
            return
          }
        }
      } catch (e) {
        if (stale()) return
        if (e instanceof ApiError && e.code === 'stale_head') showToast(e.message, { label: 'Refresh', run: onRefresh })
        else showToast(errorMessage(e))
      } finally {
        if (current.current === run) {
          current.current = null
          setIndex(null)
          setLooking(false)
        }
      }
    },
    [indexed, open, dismiss],
  )

  const request = useCallback(
    (source: NavSource) => {
      dismiss()
      const run: Run = { abort: new AbortController() }
      current.current = run
      setLooking(true)
      setLookId((id) => id + 1)
      void lookUp(run, source)
    },
    [dismiss, lookUp],
  )

  return { request, dismiss, open: index !== null || list !== null, looking: looking && index === null, lookId, index, list }
}
