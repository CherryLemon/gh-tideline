import { forwardRef, useCallback, useEffect, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { DiffEditor, Editor } from '@monaco-editor/react'
import type * as Monaco from 'monaco-editor/editor/editor.api'
import type { ChangedFile, RepoRef, ReviewThread, TreeEntry } from '../api'
import { errorMessage } from '../api'
import { SANDY_BEACH_THEME } from '../monaco/theme'
import { ZoneHost } from '../monaco/zones'
import { LIMITS, isGitlink, shortSha } from '../degrade'
import { formatBytes, loadFile, loadUnchanged, models, prefetchFile, type LoadedFile, type SideInfo } from '../monaco/files'
import { placeThreads, type PanelReason, type Placement } from '../threads'
import { isChanged } from '../tree'
import { ImageDiff, ImageView } from './ImageDiff'
import { PaneThreads } from './PaneThreads'

export interface ChangeInfo {
  /** 0-based index of the change at the caret, or null when the caret is between changes. */
  index: number | null
  total: number
}

export interface PendingJump {
  path: string
  where: 'first' | 'last'
}

/**
 * - moved: went to another change in this file;
 * - none: no further change in this direction (the caller moves to the next/previous file);
 * - notReady: the file or its diff is still loading; the move is queued and applied once ready.
 */
export type StepResult = 'moved' | 'none' | 'notReady'

export interface DiffPaneHandle {
  nextChange(): StepResult
  prevChange(): StepResult
}

/** The review threads of the file shown, and how to draw them. */
export interface PaneReview {
  /** The file's threads (`threadsForFile`). */
  threads: readonly ReviewThread[]
  /** Whether the threads were read at the head the page shows; if not, none go under lines. */
  headOk: boolean
  /** A thread's card; `reason` is set when it is listed above the diff instead of under its line. */
  renderThread(thread: ReviewThread, reason?: PanelReason): React.ReactNode
  /** Shown above the listed threads. */
  notice?: React.ReactNode
}

interface Props {
  repo: RepoRef
  /** A changed file (diffed), or an unchanged file of the head tree (shown read-only). */
  file: ChangedFile | TreeEntry
  /**
   * `file` (unchanged) comes from an older head's listing and may differ at the shown head:
   * stay loading, without loading it, until the page has the shown head's entry.
   */
  provisional?: boolean
  nextFile: ChangedFile | null
  sideBySide: boolean
  ignoreTrimWhitespace: boolean
  /** Set by the page before switching files with F7 / Shift+F7; applied once the diff is computed. */
  pendingJump: React.MutableRefObject<PendingJump | null>
  /** Controls shown at the right end of the file header. */
  headerActions?: React.ReactNode
  /** Absent while the threads haven't been read. */
  review?: PaneReview
  onChangeInfo(info: ChangeInfo | null): void
}

type PaneState =
  | { kind: 'loading' }
  | { kind: 'error'; message: string }
  | Exclude<LoadedFile, { kind: 'text' } | { kind: 'single' }>
  | { kind: 'text' | 'single'; plainReason: string | null }

/** Which editor shows a file: the diff editor, or the plain one for unchanged files. */
type Attached = 'diff' | 'single' | null

/** Where the threads of `path` went; `diff`: placed against a computed diff. */
interface Placed {
  path: string
  diff: boolean
  placement: Placement
}

/** Diff limits for the attached file; a force-loaded file has none. */
function diffLimits(forced: boolean): Pick<Monaco.editor.IDiffEditorOptions, 'maxComputationTime' | 'maxFileSize'> {
  return {
    maxComputationTime: forced ? 0 : LIMITS.maxComputationTimeMs,
    maxFileSize: forced ? 0 : LIMITS.maxFileSizeMb,
  }
}

type DiffEditorT = Monaco.editor.IStandaloneDiffEditor
type EditorT = Monaco.editor.IStandaloneCodeEditor

const EDITOR_OPTIONS = {
  readOnly: true,
  domReadOnly: true,
  fontFamily: "'JetBrains Mono', ui-monospace, monospace",
  fontSize: 13,
  lineHeight: 20,
  fontLigatures: false,
  minimap: { enabled: false },
  scrollBeyondLastLine: false,
  stickyScroll: { enabled: false },
  renderLineHighlight: 'line',
  automaticLayout: true,
  scrollbar: { verticalScrollbarSize: 12, horizontalScrollbarSize: 12, useShadows: false },
  padding: { top: 4 },
} satisfies Monaco.editor.IEditorOptions

const LOADING: PaneState = { kind: 'loading' }

/** Line a change is anchored at on one side (pure insertions/deletions have end = 0). */
function anchor(c: Monaco.editor.ILineChange, side: 'original' | 'modified'): number {
  const start = side === 'original' ? c.originalStartLineNumber : c.modifiedStartLineNumber
  return Math.max(1, start)
}

function endLine(c: Monaco.editor.ILineChange, side: 'original' | 'modified'): number {
  const end = side === 'original' ? c.originalEndLineNumber : c.modifiedEndLineNumber
  return end === 0 ? anchor(c, side) : end
}

export const DiffPane = forwardRef<DiffPaneHandle, Props>(function DiffPane(
  { repo, file, provisional = false, nextFile, sideBySide, ignoreTrimWhitespace, pendingJump, headerActions, review, onChangeInfo },
  ref,
) {
  const editorRef = useRef<DiffEditorT | null>(null)
  const [diffEditor, setDiffEditor] = useState<DiffEditorT | null>(null)
  const editorReady = diffEditor !== null
  const changed = isChanged(file) ? file : null
  // The plain editor is mounted on the first unchanged file and kept afterwards.
  const singleRef = useRef<EditorT | null>(null)
  const [singleWanted, setSingleWanted] = useState(false)
  if (!changed && !singleWanted) setSingleWanted(true)
  const [singleReady, setSingleReady] = useState(false)
  /**
   * Load state tagged with its file: until the load effect has run for a new file, it is loading.
   * A provisional file is loading too, whatever was loaded for its path before.
   */
  const [loaded, setLoaded] = useState<{ path: string | null; state: PaneState }>({ path: null, state: LOADING })
  const state = !provisional && loaded.path === file.path ? loaded.state : LOADING
  const [forced, setForced] = useState<ReadonlySet<string>>(() => new Set())
  const [retry, setRetry] = useState(0)
  /** Which editor has some file's models attached; while the next file loads, its content stays visible. */
  const [attached, setAttached] = useState<Attached>(null)
  /** Whether the attached file was force-loaded; part of `options` so re-applied options keep its limits. */
  const [attachedForced, setAttachedForced] = useState(false)

  /** Path whose models are currently attached to the editor. */
  const shownPath = useRef<string | null>(null)
  /** Per path: its view state, and whether the user had moved the caret in it (see `fresh`). */
  const viewStates = useRef(new Map<string, { view: Monaco.editor.IDiffEditorViewState; fresh: boolean }>())
  /** As above, for the plain editor. */
  const singlePath = useRef<string | null>(null)
  const singleViewStates = useRef(new Map<string, Monaco.editor.ICodeEditorViewState>())
  /** Index of the change we last navigated to (valid while the caret stays on it). */
  const navIndex = useRef<number | null>(null)
  /** True until the user moves the caret in a freshly opened file: F7 then goes to change #1 even at line 1. */
  const fresh = useRef(true)
  const onChangeInfoRef = useRef(onChangeInfo)
  onChangeInfoRef.current = onChangeInfo
  /** The thread cards in the diff editor. */
  const [host, setHost] = useState<ZoneHost | null>(null)
  /** Bumped when the diff editor has a new diff, so the cards are placed against it. */
  const [diffTick, setDiffTick] = useState(0)

  const options = useMemo<Monaco.editor.IDiffEditorConstructionOptions>(
    () => ({
      originalEditable: false,
      renderSideBySide: sideBySide,
      useInlineViewWhenSpaceIsLimited: false,
      ignoreTrimWhitespace,
      hideUnchangedRegions: { enabled: true, contextLineCount: 3, minimumLineCount: 3, revealLineCount: 20 },
      diffAlgorithm: 'advanced',
      experimental: { showMoves: true },
      renderOverviewRuler: true,
      ...diffLimits(attachedForced),
      ...EDITOR_OPTIONS,
      renderMarginRevertIcon: false,
      renderGutterMenu: false,
    }),
    [sideBySide, ignoreTrimWhitespace, attachedForced],
  )

  const activeSide = useCallback((ed: DiffEditorT): 'original' | 'modified' => {
    return sideBySide && ed.getOriginalEditor().hasTextFocus() ? 'original' : 'modified'
  }, [sideBySide])

  const reportChangeInfo = useCallback(() => {
    const ed = editorRef.current
    const changes = ed?.getLineChanges()
    if (!ed || !changes) {
      onChangeInfoRef.current(null)
      return
    }
    const side = activeSide(ed)
    const editor = side === 'original' ? ed.getOriginalEditor() : ed.getModifiedEditor()
    const line = editor.getPosition()?.lineNumber ?? 1
    let index: number | null = null
    const nav = navIndex.current
    if (nav !== null && nav < changes.length && anchor(changes[nav], side) === line) index = nav
    else if (!fresh.current) {
      const hit = changes.findIndex((c) => line >= anchor(c, side) && line <= endLine(c, side))
      index = hit >= 0 ? hit : null
    }
    onChangeInfoRef.current({ index, total: changes.length })
  }, [activeSide])

  const jumpTo = useCallback(
    (index: number, side: 'original' | 'modified' = 'modified') => {
      const ed = editorRef.current
      const changes = ed?.getLineChanges()
      if (!ed || !changes || index < 0 || index >= changes.length) return
      const editor = side === 'original' ? ed.getOriginalEditor() : ed.getModifiedEditor()
      const line = anchor(changes[index], side)
      navIndex.current = index
      fresh.current = false
      editor.setPosition({ lineNumber: line, column: 1 })
      editor.revealLineInCenterIfOutsideViewport(line)
      editor.focus()
      reportChangeInfo()
    },
    [reportChangeInfo],
  )

  /** Apply a queued F7 jump once the diff of its file is computed; true when it moved the caret. */
  const consumePendingJump = useCallback((): boolean => {
    const ed = editorRef.current
    const pj = pendingJump.current
    if (!ed || !pj || pj.path !== shownPath.current) return false
    const changes = ed.getLineChanges()
    if (!changes) return false
    pendingJump.current = null
    if (changes.length === 0) return false
    jumpTo(pj.where === 'first' ? 0 : changes.length - 1)
    return true
  }, [jumpTo, pendingJump])

  const step = useCallback(
    (dir: 1 | -1): StepResult => {
      const ed = editorRef.current
      const ready = state.kind === 'text' && shownPath.current === file.path
      // Anything but a text diff has no changes to step through.
      if (!changed || (state.kind !== 'loading' && state.kind !== 'text')) return 'none'
      const changes = ready ? ed?.getLineChanges() : null
      if (!ed || !changes) {
        // Don't let the caller skip a file whose diff isn't there yet: land on its first/last change later.
        pendingJump.current = { path: file.path, where: dir === 1 ? 'first' : 'last' }
        return 'notReady'
      }
      if (changes.length === 0) return 'none'
      const side = activeSide(ed)
      const editor = side === 'original' ? ed.getOriginalEditor() : ed.getModifiedEditor()
      const line = editor.getPosition()?.lineNumber ?? 1
      let target: number
      if (fresh.current) {
        target = dir === 1 ? 0 : -1
      } else {
        const nav = navIndex.current
        const onNav = nav !== null && nav < changes.length && anchor(changes[nav], side) === line
        if (onNav) target = nav + dir
        else if (dir === 1) target = changes.findIndex((c) => anchor(c, side) > line)
        else target = changes.findLastIndex((c) => anchor(c, side) < line)
      }
      if (target < 0 || target >= changes.length) return 'none'
      jumpTo(target, side)
      return 'moved'
    },
    [activeSide, changed, file.path, jumpTo, pendingJump, state.kind],
  )

  useImperativeHandle(ref, () => ({ nextChange: () => step(1), prevChange: () => step(-1) }), [step])

  const handleMount = useCallback(
    (ed: DiffEditorT) => {
      editorRef.current = ed
      ed.onDidUpdateDiff(() => {
        if (!ed.getLineChanges()) return
        if (!consumeRef.current()) reportRef.current()
        setDiffTick((t) => t + 1)
      })
      const onCursor = (e: Monaco.editor.ICursorPositionChangedEvent) => {
        if (e.source === 'mouse' || e.source === 'keyboard') {
          fresh.current = false
          navIndex.current = null
        }
        reportRef.current()
      }
      ed.getModifiedEditor().onDidChangeCursorPosition(onCursor)
      ed.getOriginalEditor().onDidChangeCursorPosition(onCursor)
      setDiffEditor(ed)
    },
    [],
  )
  // The mount callback is registered once; route it to the latest closures.
  const consumeRef = useRef(consumePendingJump)
  consumeRef.current = consumePendingJump
  const reportRef = useRef(reportChangeInfo)
  reportRef.current = reportChangeInfo

  useEffect(() => {
    if (!diffEditor) return
    const h = new ZoneHost(diffEditor)
    setHost(h)
    return () => {
      h.dispose()
      setHost(null)
    }
  }, [diffEditor])

  const handleSingleMount = useCallback((ed: EditorT) => {
    singleRef.current = ed
    // @monaco-editor/react attached an empty model of its own; files are set per load.
    ed.getModel()?.dispose()
    setSingleReady(true)
  }, [])

  /** Take the models off the diff editor, keeping its view state; the caller updates in-use models. */
  const detachDiff = useCallback(() => {
    const ed = editorRef.current
    if (!ed) return
    if (shownPath.current !== null && ed.getModel()) {
      const vs = ed.saveViewState()
      if (vs) viewStates.current.set(shownPath.current, { view: vs, fresh: fresh.current })
    }
    shownPath.current = null
    ed.setModel(null)
  }, [])

  const detachSingle = useCallback(() => {
    const ed = singleRef.current
    if (!ed) return
    if (singlePath.current !== null && ed.getModel()) {
      const vs = ed.saveViewState()
      if (vs) singleViewStates.current.set(singlePath.current, vs)
    }
    singlePath.current = null
    ed.setModel(null)
  }, [])

  const detach = useCallback(() => {
    detachDiff()
    detachSingle()
    models.setInUse([])
    setAttached(null)
  }, [detachDiff, detachSingle])

  const isForced = forced.has(file.path)

  // An unchanged file waits for the plain editor too.
  const ready = editorReady && (!!changed || singleReady)
  useEffect(() => {
    if (!ready) return
    let cancelled = false
    let imageUrls: string[] = []
    const setState = (st: PaneState) => setLoaded({ path: file.path, state: st })
    setState(LOADING)
    onChangeInfoRef.current(null)
    if (provisional) return
    const load = isChanged(file) ? loadFile(repo, file, isForced) : loadUnchanged(repo, file, isForced)
    load
      .then((res) => {
        if (res.kind === 'image') imageUrls = [res.baseUrl, res.headUrl].filter((u): u is string => !!u)
        if (cancelled) {
          imageUrls.forEach((u) => URL.revokeObjectURL(u))
          if (res.kind === 'text' || res.kind === 'single') res.release()
          return
        }
        const ed = editorRef.current!
        if (res.kind === 'single') {
          const se = singleRef.current!
          detachDiff()
          const prevPath = singlePath.current
          // Includes the same path (refresh, retry): its scroll position is the one to keep.
          if (prevPath !== null && se.getModel()) {
            const vs = se.saveViewState()
            if (vs) singleViewStates.current.set(prevPath, vs)
          }
          singlePath.current = file.path
          const unchanged = se.getModel() === res.model
          if (!unchanged) se.setModel(res.model)
          models.setInUse([res.model])
          res.release()
          if (!(unchanged && prevPath === file.path)) {
            const vs = singleViewStates.current.get(file.path)
            if (vs) se.restoreViewState(vs)
            else se.setScrollTop(0)
          }
          setAttached('single')
          setState({ kind: 'single', plainReason: res.plainReason })
          onChangeInfoRef.current({ index: null, total: 0 })
        } else if (res.kind !== 'text') {
          detach()
          if (pendingJump.current?.path === file.path) pendingJump.current = null
          setState(res)
        } else {
          detachSingle()
          const prevPath = shownPath.current
          const current = ed.getModel()
          // Includes the same path (refresh, retry): its scroll position is the one to keep.
          if (prevPath !== null && current) {
            const vs = ed.saveViewState()
            if (vs) viewStates.current.set(prevPath, { view: vs, fresh: fresh.current })
          }
          // Set before setModel so a synchronously delivered diff update already sees the new path.
          shownPath.current = file.path
          const unchanged = !!current && current.original === res.original && current.modified === res.modified
          if (!unchanged) {
            // Before setModel, so the first diff of this file is computed with its own limits.
            ed.updateOptions(diffLimits(res.forced))
            ed.setModel({ original: res.original, modified: res.modified })
          }
          models.setInUse([res.original, res.modified])
          res.release()
          if (!(unchanged && prevPath === file.path)) {
            navIndex.current = null
            const saved = viewStates.current.get(file.path)
            // The cards put the caret on the first change; F7 still goes there first.
            fresh.current = !saved || saved.fresh
            if (saved) ed.restoreViewState(saved.view)
            else ed.getModifiedEditor().setScrollTop(0)
          }
          setAttachedForced(res.forced)
          setAttached('diff')
          setState({ kind: 'text', plainReason: res.plainReason })
          if (!consumePendingJump()) reportChangeInfo()
        }
        if (nextFile) prefetchFile(repo, nextFile)
      })
      .catch((e: unknown) => {
        if (cancelled) return
        detach()
        setState({ kind: 'error', message: errorMessage(e) })
      })
    return () => {
      cancelled = true
      imageUrls.forEach((u) => URL.revokeObjectURL(u))
    }
    // `nextFile` is read only for prefetching after a load; changing it must not reload.
  }, [ready, repo, file, provisional, isForced, retry])

  // Threads go under their lines when the file is shown as a text diff, else above it. While
  // another file loads there are none; while this one reloads (Refresh, retry) they stay put.
  const oldPath = changed?.oldPath !== undefined && changed.oldPath !== changed.path ? changed.oldPath : undefined
  const threads = review?.threads
  const headOk = review?.headOk ?? true
  const lastPlaced = useRef<Placed | null>(null)
  const placed = useMemo<Placed | null>(() => {
    if (!threads) return null
    if (state.kind === 'loading') return lastPlaced.current?.path === file.path ? lastPlaced.current : null
    const ed = editorRef.current
    const model = state.kind === 'text' && attached === 'diff' ? ed?.getModel() : null
    const lineChanges = model ? (ed?.getLineChanges() ?? null) : null
    // The new content of a reloaded file is shown before its diff is computed.
    if (model && !lineChanges && lastPlaced.current?.path === file.path && lastPlaced.current.diff) return lastPlaced.current
    const placement = placeThreads({
      threads,
      oldPath,
      view: model ? (sideBySide ? 'split' : 'unified') : null,
      lineChanges,
      lineCounts: { original: model?.original.getLineCount() ?? 0, modified: model?.modified.getLineCount() ?? 0 },
      headOk,
    })
    return { path: file.path, diff: lineChanges !== null, placement }
    // `diffTick` stands for the editor's diff, read above.
  }, [threads, file.path, oldPath, state.kind, attached, sideBySide, headOk, diffTick])
  const placement = placed?.placement ?? null

  useLayoutEffect(() => {
    lastPlaced.current = placed
    if (!host) return
    const specs = (placement?.zones ?? []).map((z) => ({
      key: z.thread.id,
      editor: z.editor,
      afterLine: z.afterLine,
      ordinal: z.ordinal,
      reveal: z.reveal,
    }))
    // Unfolding moves the caret; in a file the user hasn't moved in, leave it on the first change.
    const first = editorRef.current?.getLineChanges()?.[0]
    host.sync(specs, fresh.current ? (first ? anchor(first, 'modified') : 1) : 'keep')
  }, [placed, placement, host])

  // Multi-line threads mark their lines.
  const ranges = useRef<{ original: Monaco.editor.IEditorDecorationsCollection; modified: Monaco.editor.IEditorDecorationsCollection } | null>(null)
  useEffect(() => {
    const ed = editorRef.current
    if (!ed) return
    ranges.current ??= {
      original: ed.getOriginalEditor().createDecorationsCollection(),
      modified: ed.getModifiedEditor().createDecorationsCollection(),
    }
    for (const side of ['original', 'modified'] as const) {
      const marks: Monaco.editor.IModelDeltaDecoration[] = []
      for (const z of placement?.zones ?? []) {
        if (z.editor !== side || !z.highlight || z.thread.isResolved) continue
        marks.push({
          range: { startLineNumber: z.highlight.from, startColumn: 1, endLineNumber: z.highlight.to, endColumn: 1 },
          options: { isWholeLine: true, className: 'tl-thread-range' },
        })
      }
      ranges.current[side].set(marks)
    }
  }, [placement])

  const cards =
    review && host && placement
      ? placement.zones.map((z) => createPortal(review.renderThread(z.thread), host.node(z.thread.id), z.thread.id))
      : null

  // The attached content may be stale at the new head of a provisional file: show neither editor.
  const showEditor = !provisional && (state.kind === 'text' || (state.kind === 'loading' && attached === 'diff'))
  const showSingle = !provisional && (state.kind === 'single' || (state.kind === 'loading' && attached === 'single'))
  const overlay = renderOverlay(state, file, {
    load: () => setForced((s) => new Set(s).add(file.path)),
    retry: () => setRetry((n) => n + 1),
  })

  return (
    <div className="diff-pane">
      <FileHeader
        file={file}
        plainReason={state.kind === 'text' || state.kind === 'single' ? state.plainReason : null}
        actions={headerActions}
      />
      {review && placement && <PaneThreads items={placement.panel} renderThread={review.renderThread} notice={review.notice} />}
      <div className="diff-body">
        {singleWanted && (
          <div className={`diff-editor single-editor${showSingle ? '' : ' is-hidden'}`} data-testid="single-editor">
            <Editor
              theme={SANDY_BEACH_THEME}
              options={EDITOR_OPTIONS}
              keepCurrentModel
              onMount={handleSingleMount}
              loading={<div className="pane-msg">Loading editor…</div>}
            />
          </div>
        )}
        <div className={`diff-editor${showEditor ? '' : ' is-hidden'}`}>
          <DiffEditor
            theme={SANDY_BEACH_THEME}
            options={options}
            keepCurrentOriginalModel
            keepCurrentModifiedModel
            onMount={handleMount}
            loading={<div className="pane-msg">Loading editor…</div>}
          />
        </div>
        {overlay}
      </div>
      {cards}
    </div>
  )
})

function sideSize(s: SideInfo): string {
  return s.bytes === null ? '(none)' : formatBytes(s.bytes) + (s.lines ? `, ${s.lines.toLocaleString()} ${s.lines === 1 ? 'line' : 'lines'}` : '')
}

/** Both sides of a changed file; only the head of an unchanged one. */
function sizeLine(file: ChangedFile | TreeEntry, base: SideInfo, head: SideInfo): string {
  return isChanged(file) ? `${sideSize(base)} → ${sideSize(head)}` : sideSize(head)
}

function renderOverlay(state: PaneState, file: ChangedFile | TreeEntry, actions: { load(): void; retry(): void }) {
  switch (state.kind) {
    case 'text':
    case 'single':
      return null
    case 'loading':
      return <div className="pane-msg pane-loading">Loading…</div>
    case 'error':
      return (
        <div className="pane-msg">
          <div className="pane-error">{state.message}</div>
          <button className="btn" onClick={actions.retry}>Retry</button>
        </div>
      )
    case 'binary':
      return (
        <div className="pane-msg">
          <div className="pane-title">Binary file</div>
          <div className="pane-sub">Size {sizeLine(file, state.base, state.head)}</div>
        </div>
      )
    case 'deferred':
      return (
        <div className="pane-msg">
          <div className="pane-title">File too large to load automatically</div>
          <div className="pane-sub">{sizeLine(file, state.base, state.head)}</div>
          <div className="pane-sub">Files over 5 MB or 50,000 lines load on request, without syntax highlighting.</div>
          <button className="btn btn-primary" onClick={actions.load}>Load file</button>
        </div>
      )
    case 'image':
      return isChanged(file) ? <ImageDiff key={file.path} file={file} state={state} /> : <ImageView key={file.path} state={state} />
    case 'submodule':
      return isChanged(file) ? <SubmoduleDiff file={file} /> : <SubmoduleEntry entry={file} />
  }
}

/** An unchanged gitlink: the commit the head records. */
function SubmoduleEntry({ entry }: { entry: TreeEntry }) {
  return (
    <div className="pane-msg" data-testid="submodule-diff">
      <div className="pane-title">Submodule</div>
      <div className="pane-sub submodule-line">
        <SubmoduleSide oid={entry.oid} mode={entry.mode} />
      </div>
    </div>
  )
}

function SubmoduleSide({ oid, mode }: { oid: string | null; mode: string }) {
  if (!oid) return <span className="muted">(none)</span>
  return (
    <code title={oid}>
      {isGitlink(mode) ? 'Subproject commit ' : 'File '}
      {shortSha(oid)}
    </code>
  )
}

/** Gitlink entry: only the recorded commit ids, nothing is fetched (they live in another repository). */
function SubmoduleDiff({ file }: { file: ChangedFile }) {
  return (
    <div className="pane-msg" data-testid="submodule-diff">
      <div className="pane-title">Submodule</div>
      <div className="pane-sub submodule-line">
        {file.oldOid && file.newOid ? (
          <>
            <SubmoduleSide oid={file.oldOid} mode={file.oldMode} />
            <span className="arrow"> → </span>
            <SubmoduleSide oid={file.newOid} mode={file.newMode} />
          </>
        ) : (
          <SubmoduleSide oid={file.oldOid ?? file.newOid} mode={file.oldOid ? file.oldMode : file.newMode} />
        )}
      </div>
      {(!file.oldOid || !file.newOid) && <div className="pane-sub">{file.oldOid ? 'Removed' : 'Added'}</div>}
    </div>
  )
}

function FileHeader({ file, plainReason, actions }: { file: ChangedFile | TreeEntry; plainReason: string | null; actions?: React.ReactNode }) {
  if (!isChanged(file)) {
    return (
      <div className="file-header">
        <span className="file-header-path" title={file.path}>
          {file.path}
        </span>
        <span className="chip" title="This pull request doesn't change this file: showing its content at the head commit">
          Unchanged
        </span>
        {plainReason && <span className="chip chip-warn">{plainReason}</span>}
        <span className="spacer" />
        {actions}
      </div>
    )
  }
  const renamed = file.oldPath && file.oldPath !== file.path
  const modeChanged = file.oldMode !== file.newMode && file.oldMode !== '000000' && file.newMode !== '000000'
  return (
    <div className="file-header">
      <span className={`status-letter st-${file.status}`}>{file.status}</span>
      <span className="file-header-path" title={file.path}>
        {renamed ? (
          <>
            <span className="muted">{file.oldPath}</span>
            <span className="arrow"> → </span>
            {file.path}
          </>
        ) : (
          file.path
        )}
      </span>
      {modeChanged && (
        <span className="chip" title="File mode changed">
          {file.oldMode} → {file.newMode}
        </span>
      )}
      {file.generated && <span className="chip">generated</span>}
      {plainReason && <span className="chip chip-warn">{plainReason}</span>}
      <span className="spacer" />
      {actions}
    </div>
  )
}
