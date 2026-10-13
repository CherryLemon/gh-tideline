import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react'
import {
  ApiError,
  STALE_HEAD_MESSAGE,
  createThread,
  getPr,
  getPrFiles,
  getPrTree,
  getThreads,
  errorMessage,
  setThreadResolved,
  setViewed,
  type ChangedFile,
  type CommentMode,
  type NewThreadTarget,
  type PrSummary,
  type RepoRef,
  type ReviewThread,
  type TreeEntry,
  type ViewedState,
} from '../api'
import { fileQuery, navigate, type PrRoute } from '../router'
import {
  GENERATED_KEY,
  adjacentChanged,
  ancestorKeys,
  defaultExpanded,
  isChanged,
  prepareFullTree,
  prepareTree,
  treeRows,
  type TreeRow,
} from '../tree'
import { usePersistentState } from '../storage'
import { ViewedSync, isTypingTarget, nextUnviewed, viewedMap } from '../viewed'
import { fileCounts, threadsForFile, type PanelReason } from '../threads'
import { useReview, type ReviewRead } from '../useReview'
import { DraftStore, draftKey, sessionDraftStorage, type DraftTarget } from '../drafts'
import { postThread } from '../compose'
import { DiffPane, type ChangeInfo, type DiffPaneHandle, type PaneReview, type PendingJump } from './DiffPane'
import { FileTree, type ListingState, type TreeScope } from './FileTree'
import { FileFinder } from './FileFinder'
import { Toast, type ToastMessage } from './Toast'
import { ThreadCard } from './ThreadCard'
import { Composer } from './Composer'

interface Props {
  route: PrRoute
  search: string
}

/**
 * `requestedAt`: `performance.now()` when the requests were sent (see `ViewedSync.rebase`).
 * `threads`: read alongside, but the page doesn't wait for it (nor fail with it).
 */
type Data = { pr: PrSummary; files: ChangedFile[]; requestedAt: number; threads: ReviewRead }

const DOUBLE_SHIFT_MS = 350

/** The head tree of one head: its entries, or why listing it failed. */
type Listing = { head: string; entries: TreeEntry[]; error: null } | { head: string; entries: null; error: string; stale: boolean }

/**
 * A listing that takes longer than this is given up on, so that a request that never
 * returns ends in an error the user can retry. Normally it takes seconds at most.
 */
const LISTING_TIMEOUT_MS = 60_000
const LISTING_TIMEOUT = `Listing the project files timed out after ${LISTING_TIMEOUT_MS / 1000} s`

/**
 * `v`, also with Caps Lock on or a non-Latin layout (then by physical key). Not while an
 * IME composes (`Process`, keyCode 229): that `v` is input.
 */
function isVKey(e: KeyboardEvent): boolean {
  if (e.isComposing || e.keyCode === 229) return false
  if (e.key === 'v' || e.key === 'V') return true
  return e.key.length === 1 && !/^[a-z]$/i.test(e.key) && e.code === 'KeyV'
}

export function PrPage({ route, search }: Props) {
  const repo = useMemo<RepoRef>(
    () => ({ host: route.host, owner: route.owner, repo: route.repo }),
    [route.host, route.owner, route.repo],
  )
  const [data, setData] = useState<Data | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [refreshing, setRefreshing] = useState(false)

  const load = useCallback(
    async (refresh: boolean): Promise<Data> => {
      const requestedAt = performance.now()
      const threads = getThreads(repo, route.number)
      // Handled by the page once it shows; not if loading the pull request fails.
      threads.catch(() => {})
      const [pr, files] = await Promise.all([getPr(repo, route.number, refresh), getPrFiles(repo, route.number, refresh)])
      return { pr, files, requestedAt, threads: { promise: threads, requestedAt } }
    },
    [repo, route.number],
  )

  useEffect(() => {
    let cancelled = false
    setData(null)
    setError(null)
    load(false).then(
      (d) => !cancelled && setData(d),
      (e: unknown) => !cancelled && setError(errorMessage(e)),
    )
    return () => {
      cancelled = true
    }
  }, [load])

  // The newest refresh wins, so overlapping ones cannot bring back an older list.
  const refreshSeq = useRef(0)
  const refresh = useCallback(() => {
    const seq = ++refreshSeq.current
    const latest = () => seq === refreshSeq.current
    setRefreshing(true)
    load(true)
      .then(
        (d) => latest() && setData(d),
        (e: unknown) => latest() && setError(errorMessage(e)),
      )
      .finally(() => latest() && setRefreshing(false))
  }, [load])

  useEffect(() => {
    if (data) document.title = `#${data.pr.number} ${data.pr.title} · ${data.pr.owner}/${data.pr.repo}`
  }, [data])

  if (error) {
    return (
      <div className="page-message">
        <div className="page-message-box">
          <div className="page-message-title">Can't open the pull request</div>
          <div className="page-message-body">{error}</div>
          <a className="btn" href="/">Back to home</a>
        </div>
      </div>
    )
  }
  if (!data) {
    return (
      <div className="page-message">
        <div className="muted">Opening {route.owner}/{route.repo} #{route.number}…</div>
      </div>
    )
  }
  return <PrView repo={repo} data={data} search={search} onRefresh={refresh} refreshing={refreshing} />
}

interface ViewProps {
  repo: RepoRef
  data: Data
  search: string
  refreshing: boolean
  onRefresh(): void
}

function PrView({ repo, data, search, refreshing, onRefresh }: ViewProps) {
  const { pr, files, requestedAt } = data
  const head = pr.head.sha
  const [storedScope, setScope] = usePersistentState<TreeScope>('dr.treeScope', 'changed')
  const scope: TreeScope = storedScope === 'all' ? 'all' : 'changed'

  // The head tree is listed only when needed: for the full tree, or to open a file the PR
  // didn't change (a reloaded ?file=). One listing per head; while a new head's listing
  // loads, the previous one stands in, so the tree and an open unchanged file stay put.
  const [listing, setListing] = useState<Listing | null>(null)
  const haveListing = listing?.head === head
  const requested = new URLSearchParams(search).get('file')
  const changedByPath = useMemo(() => new Map(files.map((f) => [f.path, f])), [files])
  const changedCurrent = requested === null ? null : (changedByPath.get(requested) ?? null)
  const needListing = scope === 'all' || (requested !== null && !changedCurrent)
  const entries = haveListing || needListing ? (listing?.entries ?? null) : null
  const headNow = useRef(head)
  headNow.current = head
  const listingFor = useRef<string | null>(null)
  useEffect(() => {
    if (!needListing || haveListing || listingFor.current === head) return
    listingFor.current = head
    const abort = new AbortController()
    const timer = setTimeout(() => abort.abort(), LISTING_TIMEOUT_MS)
    const done = (l: Listing) => {
      clearTimeout(timer)
      if (listingFor.current === head) listingFor.current = null
      // A refresh moved on to another head meanwhile: that one gets its own listing.
      if (headNow.current === head) setListing(l)
    }
    getPrTree(repo, pr.number, head, abort.signal)
      .then((t) =>
        // The server already serves a newer head than this page shows.
        done(t.head === head ? { head, entries: t.entries, error: null } : { head, entries: null, error: STALE_HEAD_MESSAGE, stale: true }),
      )
      // Also catches a malformed response, so the listing never stays pending.
      .catch((e: unknown) => done({ head, entries: null, error: abort.signal.aborted ? LISTING_TIMEOUT : errorMessage(e), stale: false }))
  }, [needListing, haveListing, head, repo, pr.number])
  const retryListing = useCallback(() => setListing(null), [])
  // A refresh (even to the same head) also retries a listing that failed.
  const listedAt = useRef(requestedAt)
  useEffect(() => {
    if (listedAt.current === requestedAt) return
    listedAt.current = requestedAt
    setListing((l) => (l && l.error !== null && l.head === head ? null : l))
  }, [requestedAt, head])

  const changedTree = useMemo(() => prepareTree(files), [files])
  const fullTree = useMemo(() => (entries ? prepareFullTree(files, entries) : null), [files, entries])
  const entryByPath = useMemo(() => (entries ? new Map(entries.map((e) => [e.path, e])) : null), [entries])
  const shownTree = scope === 'all' ? fullTree : changedTree
  /** Changed files in the order of the tree shown (the full tree has no Generated group). */
  const order = useMemo(() => (shownTree ?? changedTree).all.filter(isChanged), [shownTree, changedTree])
  const totalFiles = entries ? entries.length : null

  // Folder state per scope: the changed tree starts expanded (but Generated), the full tree
  // starts with only the folders leading to changes open.
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set([GENERATED_KEY]))
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => defaultExpanded(files))
  const seededHead = useRef(head)
  useEffect(() => {
    if (seededHead.current === head) return
    seededHead.current = head
    // A new head can change files elsewhere: open their folders too, keep the rest as is.
    setExpanded((prev) => new Set([...prev, ...defaultExpanded(files)]))
  }, [head, files])
  const rows = useMemo<TreeRow[]>(() => {
    if (scope === 'changed') return treeRows(changedTree, (k) => !collapsed.has(k))
    return fullTree ? treeRows(fullTree, (k) => expanded.has(k)) : []
  }, [scope, changedTree, fullTree, collapsed, expanded])
  const listingState: ListingState =
    scope !== 'all' || fullTree
      ? null
      : haveListing && listing.error !== null
        ? { kind: 'error', message: listing.error, stale: listing.stale }
        : { kind: 'loading' }

  const [sideBySide, setSideBySide] = usePersistentState('dr.sideBySide', true)
  const [ignoreWs, setIgnoreWs] = usePersistentState('dr.ignoreTrimWhitespace', false)
  const [finderOpen, setFinderOpen] = useState(false)
  const [changeInfo, setChangeInfo] = useState<ChangeInfo | null>(null)
  const [treeWidth, setTreeWidth] = usePersistentState('dr.treeWidth', 320)

  const pendingJump = useRef<PendingJump | null>(null)
  const pane = useRef<DiffPaneHandle>(null)

  const [toast, setToast] = useState<ToastMessage | null>(null)
  const toastId = useRef(0)
  const showToast = useCallback((text: string, action?: ToastMessage['action']) => {
    setToast({ id: ++toastId.current, text, action })
  }, [])
  const dismissToast = useCallback(() => setToast(null), [])

  // Viewed marks: one sync per head, so a refresh cannot lose a request in flight; a new
  // head starts over (pending intents were made against the old one).
  const [marks, setMarks] = useState<ReadonlyMap<string, ViewedState>>(() => viewedMap(files))
  const sync = useRef<ViewedSync | null>(null)
  useEffect(() => {
    const s = new ViewedSync(
      viewedMap(files),
      (path, viewed) => setViewed(repo, pr.number, path, viewed, head),
      () => setMarks(s.snapshot()),
      (path, e) => {
        if (e instanceof ApiError && e.code === 'stale_head') showToast(e.message, { label: 'Refresh', run: onRefresh })
        else showToast(`Couldn't update the viewed state of ${path}: ${errorMessage(e)}`)
      },
    )
    sync.current = s
    setMarks(s.snapshot())
    // E.g. "The pull request has been updated" about the previous head.
    setToast(null)
    return () => {
      s.dispose()
      sync.current = null
    }
    // `files` only seeds a new sync; later lists of the same head go through rebase below.
  }, [repo, pr.number, head, showToast, onRefresh])
  useEffect(() => {
    sync.current?.rebase(viewedMap(files), requestedAt)
  }, [files, requestedAt])
  const toggleViewed = useCallback((path: string, viewed: boolean) => sync.current?.set(path, viewed), [])

  const review = useReview(
    data.threads,
    useCallback(() => getThreads(repo, pr.number), [repo, pr.number]),
    useCallback((id: string, resolved: boolean) => setThreadResolved(repo, pr.number, id, resolved), [repo, pr.number]),
    useCallback((_id: string, e: unknown) => showToast(`Couldn't update the thread: ${errorMessage(e)}`), [showToast]),
  )
  const retryReview = review.retry
  useEffect(() => {
    if (review.error) showToast(`Couldn't load the review comments: ${review.error.message}`, { label: 'Retry', run: retryReview })
  }, [review.error, retryReview, showToast])

  // Open comment boxes, per pull request; a box's text survives file switches and reloads.
  const drafts = useMemo(() => new DraftStore(`${repo.host}/${repo.owner}/${repo.repo}#${pr.number}`, sessionDraftStorage()), [repo, pr.number])
  const draftViews = useSyncExternalStore(drafts.subscribe, drafts.getSnapshot)
  const writing = useRef({ store: review.store, retry: retryReview, head })
  writing.current = { store: review.store, retry: retryReview, head }
  /** Sends the draft `key` as a new thread on `target`, at the head the page shows. */
  const sendThread = useCallback(
    (key: string, target: NewThreadTarget & { path: string }, mode: CommentMode) => {
      void drafts.send(key, async (body) => {
        const { store, retry, head } = writing.current
        if (!store) throw new Error("The review comments haven't loaded yet")
        try {
          const posted = await postThread(
            {
              store,
              create: (req) => createThread(repo, pr.number, req),
              read: () => getThreads(repo, pr.number),
              reread: retry,
              settle: () => new Promise((r) => setTimeout(r, 1000)),
            },
            { ...target, head, body, mode },
          )
          if (posted.submitError) {
            showToast(`Your comment was added to a pending review, but posting the review failed: ${posted.submitError}`)
          }
        } catch (e) {
          if (e instanceof ApiError && e.code === 'stale_head') showToast(e.message, { label: 'Refresh', run: onRefresh })
          throw e
        }
      })
    },
    [drafts, repo, pr.number, showToast, onRefresh],
  )

  const currentIndex = changedCurrent ? order.indexOf(changedCurrent) : -1
  /** A changed file, or an unchanged file of the head tree. */
  const current: ChangedFile | TreeEntry | null =
    changedCurrent ?? (requested !== null ? (entryByPath?.get(requested) ?? null) : null)
  // Still listing the head tree to find out whether ?file= is an unchanged file.
  const resolving = requested !== null && !current && needListing && !haveListing
  // An unchanged file found in the previous head's listing (standing in while this head's
  // loads): what it holds at this head is not known yet, so the pane waits.
  const provisional = current !== null && !changedCurrent && !haveListing
  // Listing it failed: say so (with a way out) rather than replace ?file= with another file.
  const unresolved = requested !== null && !current && haveListing && listing.error !== null ? listing : null

  const snapshot = review.snapshot
  const threadsHead = snapshot?.head ?? null
  const headOk = threadsHead === head
  // Read at another head while the shown head's read is on its way: line threads would land on
  // the wrong lines for a moment, so only file threads show until it arrives.
  const settling = !headOk && review.reading
  const fileThreads = useMemo<ReviewThread[] | null>(() => {
    if (!snapshot || !current) return null
    const all = isChanged(current) ? threadsForFile(snapshot.threads, current) : snapshot.threads.filter((t) => t.path === current.path)
    return settling ? all.filter((t) => t.subjectType === 'FILE') : all
  }, [snapshot, current, settling])
  const threadCounts = useMemo(() => (snapshot ? fileCounts(snapshot.threads, files) : new Map()), [snapshot, files])
  const renderThread = useCallback((thread: ReviewThread, reason?: PanelReason) => <ThreadCard thread={thread} reason={reason} />, [])

  // GitHub takes comments on the files it lists for the pull request (those with a viewed mark).
  const commentPath = changedCurrent && marks.has(changedCurrent.path) && snapshot ? changedCurrent.path : null
  const fileTarget = useMemo<DraftTarget | null>(() => (commentPath === null ? null : { kind: 'file', path: commentPath }), [commentPath])
  const fileKey = fileTarget ? draftKey(fileTarget) : null
  const fileDraft = fileKey ? draftViews.get(fileKey) : undefined
  const commentOnFile = useCallback(() => fileTarget && drafts.open(fileTarget, head), [drafts, fileTarget, head])
  const pendingReview = snapshot?.pendingReview != null
  const locked = snapshot?.locked ?? false
  const fileComposer = useMemo(
    () =>
      fileKey && fileDraft && commentPath !== null ? (
        <Composer
          key={fileKey}
          drafts={drafts}
          draftKey={fileKey}
          view={fileDraft}
          pendingReview={pendingReview}
          locked={locked}
          label={`Comment on ${commentPath}`}
          onSend={(mode) => sendThread(fileKey, { subjectType: 'FILE', path: commentPath }, mode)}
        />
      ) : undefined,
    [drafts, fileKey, fileDraft, commentPath, pendingReview, locked, sendThread],
  )

  const paneReview = useMemo<PaneReview | undefined>(() => {
    if (!fileThreads) return undefined
    const stale = !headOk && !settling && fileThreads.some((t) => t.subjectType === 'LINE')
    return {
      threads: fileThreads,
      headOk,
      renderThread,
      composer: fileComposer,
      notice: stale ? (
        <div className="pane-threads-notice">
          <span>These comments are from another version of the pull request, so they're listed here.</span>
          <button className="btn" onClick={onRefresh}>
            Refresh
          </button>
        </div>
      ) : undefined,
    }
  }, [fileThreads, headOk, settling, renderThread, fileComposer, onRefresh])

  // Without a valid ?file=, show the first file (replace, so Back leaves the PR).
  useEffect(() => {
    if (!current && !resolving && !unresolved && order.length > 0) navigate(location.pathname + fileQuery(order[0].path, location.search), true)
  }, [current, resolving, unresolved, order])

  // Reveal the current file in the tree when it changes; later collapsing its folder sticks.
  const currentPath = current?.path ?? null
  const currentGenerated = changedCurrent?.generated ?? false
  useEffect(() => {
    if (currentPath === null) return
    const keys = ancestorKeys({ path: currentPath, generated: currentGenerated })
    setCollapsed((prev) => {
      if (!keys.some((k) => prev.has(k))) return prev
      const next = new Set(prev)
      keys.forEach((k) => next.delete(k))
      return next
    })
    // The full tree has no Generated group.
    const fullKeys = ancestorKeys({ path: currentPath })
    setExpanded((prev) => (fullKeys.every((k) => prev.has(k)) ? prev : new Set([...prev, ...fullKeys])))
  }, [currentPath, currentGenerated])

  /** The changed file after (1) or before (-1) the current one, also from an unchanged file. */
  const neighbor = useCallback(
    (dir: 1 | -1): ChangedFile | null => {
      if (changedCurrent) return order[currentIndex + dir] ?? null
      if (current && fullTree) return adjacentChanged(fullTree.all, current.path, dir)
      return null
    },
    [changedCurrent, order, currentIndex, current, fullTree],
  )

  const openFile = useCallback(
    (path: string, jump?: PendingJump['where']) => {
      pendingJump.current = jump ? { path, where: jump } : null
      if (path !== requested) navigate(location.pathname + fileQuery(path, location.search))
    },
    [requested],
  )

  const toggle = useCallback(
    (key: string) => {
      const flip = (prev: ReadonlySet<string>) => {
        const next = new Set(prev)
        if (next.has(key)) next.delete(key)
        else next.add(key)
        return next
      }
      if (scope === 'all') setExpanded(flip)
      else setCollapsed(flip)
    },
    [scope],
  )

  /** `v`: toggles the current file; marking it viewed moves on to the next unviewed file. */
  const toggleCurrentViewed = () => {
    const s = sync.current
    // Unchanged files have no viewed mark.
    if (!changedCurrent || !s) return
    if (!s.has(changedCurrent.path)) {
      showToast("GitHub's file list doesn't have this path, so it can't be marked as viewed")
      return
    }
    const viewed = !s.isViewed(changedCurrent.path)
    s.set(changedCurrent.path, viewed)
    if (!viewed) return
    const next = nextUnviewed(order, currentIndex, s.snapshot())
    if (next !== null) openFile(order[next].path)
  }

  // Keep the latest navigation state for the window-level key handler.
  const nav = useRef({ neighbor, openFile, finderOpen, toggleCurrentViewed })
  useLayoutEffect(() => {
    nav.current = { neighbor, openFile, finderOpen, toggleCurrentViewed }
  })

  useEffect(() => {
    let lastShift = 0
    const onKeyDown = (e: KeyboardEvent) => {
      const { neighbor, openFile, finderOpen, toggleCurrentViewed } = nav.current
      // In a comment box (or any text field) keys are text, but Ctrl+P (the box keeps its draft).
      const typing = isTypingTarget(e.target)
      if (e.key === 'Shift') {
        if (e.repeat) return
        if (typing) {
          lastShift = 0
          return
        }
        const now = performance.now()
        if (!finderOpen && now - lastShift < DOUBLE_SHIFT_MS) {
          lastShift = 0
          setFinderOpen(true)
        } else lastShift = now
        return
      }
      lastShift = 0
      if (finderOpen) return

      const consume = () => {
        e.preventDefault()
        e.stopPropagation()
      }
      const plain = !e.altKey && !e.ctrlKey && !e.metaKey
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 'p') {
        consume()
        setFinderOpen(true)
        return
      }
      if (typing) return
      if (e.key === 'F7' && plain) {
        // Captured on window before Monaco sees it: overrides its accessible-diff-viewer binding.
        consume()
        // 'notReady' (file or diff still loading) must not advance: the pane queues the jump instead.
        const dir = e.shiftKey ? -1 : 1
        const r = (dir === 1 ? pane.current?.nextChange() : pane.current?.prevChange()) ?? 'none'
        const target = r === 'none' ? neighbor(dir) : null
        if (target) openFile(target.path, dir === 1 ? 'first' : 'last')
        return
      }
      if (e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
        consume()
        const target = neighbor(e.key === 'ArrowDown' ? 1 : -1)
        if (target) openFile(target.path)
        return
      }
      if (plain && !e.shiftKey && isVKey(e) && !e.repeat && !e.isComposing) {
        consume()
        toggleCurrentViewed()
      }
    }
    window.addEventListener('keydown', onKeyDown, true)
    return () => window.removeEventListener('keydown', onKeyDown, true)
  }, [])

  const startResize = (e: React.MouseEvent) => {
    e.preventDefault()
    const startX = e.clientX
    const startW = treeWidth
    const move = (ev: MouseEvent) => setTreeWidth(Math.min(800, Math.max(180, startW + ev.clientX - startX)))
    const up = () => {
      window.removeEventListener('mousemove', move)
      window.removeEventListener('mouseup', up)
      document.body.classList.remove('is-resizing')
    }
    document.body.classList.add('is-resizing')
    window.addEventListener('mousemove', move)
    window.addEventListener('mouseup', up)
  }

  // Memoized: from an unchanged file this scans the whole tree.
  const nextFile = useMemo(() => neighbor(1), [neighbor])

  return (
    <div className="app" style={{ '--tree-width': `${treeWidth}px` } as React.CSSProperties}>
      <header className="topbar">
        <a className="home-link" href="/" title="Home">
          Tideline
        </a>
        <span className="sep" />
        <span className="pr-repo">
          {pr.owner}/{pr.repo}
        </span>
        <a className="pr-number" href={/^https?:\/\//i.test(pr.url) ? pr.url : undefined} target="_blank" rel="noreferrer" title="Open on GitHub">
          #{pr.number}
        </a>
        <span className="pr-title" title={pr.title}>
          {pr.title}
        </span>
        <span className={`pr-state state-${pr.state.toLowerCase()}`}>{pr.state.toLowerCase()}</span>
        <span className="branches" title={`base ${pr.base.sha}\nhead ${pr.head.sha}\nmerge-base ${pr.mergeBase}`}>
          <span className="branch">{pr.base.ref}</span>
          <span className="arrow">←</span>
          <span className="branch">{pr.head.ref}</span>
        </span>
        <span className="spacer" />
        <div className="segmented" role="group" aria-label="View mode">
          <button className={sideBySide ? 'is-on' : ''} onClick={() => setSideBySide(true)} aria-pressed={sideBySide}>
            Split
          </button>
          <button className={!sideBySide ? 'is-on' : ''} onClick={() => setSideBySide(false)} aria-pressed={!sideBySide}>
            Unified
          </button>
        </div>
        <label className="check" title="Ignores whitespace only at the start and end of lines (a Monaco limit)">
          <input type="checkbox" checked={ignoreWs} onChange={(e) => setIgnoreWs(e.target.checked)} />
          Ignore whitespace
        </label>
        <button className="btn btn-icon" onClick={onRefresh} disabled={refreshing} title="Fetch the pull request again from GitHub">
          {refreshing ? 'Refreshing…' : 'Refresh'}
        </button>
      </header>

      <div className="main">
        <aside className="sidebar">
          <FileTree
            rows={rows}
            files={files}
            scope={scope}
            onScopeChange={setScope}
            totalFiles={totalFiles}
            listing={listingState}
            onRetryListing={listingState?.kind === 'error' && listingState.stale ? onRefresh : retryListing}
            currentPath={current?.path ?? null}
            viewed={marks}
            threadCounts={threadCounts}
            onOpen={openFile}
            onToggle={toggle}
            onToggleViewed={toggleViewed}
          />
        </aside>
        <div className="splitter" onMouseDown={startResize} />
        <section className="content">
          {current ? (
            <DiffPane
              ref={pane}
              repo={repo}
              file={current}
              provisional={provisional}
              nextFile={nextFile}
              sideBySide={sideBySide}
              ignoreTrimWhitespace={ignoreWs}
              pendingJump={pendingJump}
              headerActions={
                changedCurrent && (
                  <>
                    {fileTarget && (
                      <button className="btn" onClick={commentOnFile} title="Comment on the whole file">
                        Comment on file
                      </button>
                    )}
                    <ViewedCheck path={changedCurrent.path} mark={marks.get(changedCurrent.path)} onToggle={toggleViewed} />
                  </>
                )
              }
              review={paneReview}
              onChangeInfo={setChangeInfo}
            />
          ) : unresolved ? (
            <div className="pane-msg">
              <div className="pane-error">{unresolved.error}</div>
              <button className="btn" onClick={unresolved.stale ? onRefresh : retryListing}>
                {unresolved.stale ? 'Refresh' : 'Retry'}
              </button>
            </div>
          ) : (
            <div className="pane-msg muted">{resolving ? 'Loading…' : files.length === 0 ? "This pull request doesn't change any files" : ''}</div>
          )}
        </section>
      </div>

      <footer className="statusbar">
        <span className="status-item" data-testid="change-info">
          {changeInfo
            ? changeInfo.total === 0
              ? 'No changes'
              : `Change ${changeInfo.index === null ? '–' : changeInfo.index + 1}/${changeInfo.total}`
            : 'Change –'}
        </span>
        <span className="status-item">
          File {currentIndex >= 0 ? currentIndex + 1 : '–'}/{order.length}
        </span>
        <span className="spacer" />
        <span className="hint"><kbd>F7</kbd> next change</span>
        <span className="hint"><kbd>Shift+F7</kbd> previous</span>
        <span className="hint"><kbd>Alt+↓</kbd>/<kbd>Alt+↑</kbd> next/previous file</span>
        <span className="hint"><kbd>Shift Shift</kbd>/<kbd>Ctrl+P</kbd> find file</span>
        <span className="hint"><kbd>v</kbd> viewed</span>
      </footer>

      {finderOpen && (
        <FileFinder
          files={scope === 'all' && fullTree ? fullTree.all : order}
          onClose={() => setFinderOpen(false)}
          onPick={(path) => {
            setFinderOpen(false)
            openFile(path)
          }}
        />
      )}
      {toast && <Toast toast={toast} onDismiss={dismissToast} />}
    </div>
  )
}

function ViewedCheck({ path, mark, onToggle }: { path: string; mark: ViewedState | undefined; onToggle(path: string, viewed: boolean): void }) {
  if (mark === undefined) return null
  return (
    <>
      {mark === 'DISMISSED' && <span className="chip chip-dismissed">Changed since viewed</span>}
      <label className="check" title="Shortcut: v">
        <input type="checkbox" checked={mark === 'VIEWED'} onChange={(e) => onToggle(path, e.target.checked)} />
        Viewed
      </label>
    </>
  )
}
