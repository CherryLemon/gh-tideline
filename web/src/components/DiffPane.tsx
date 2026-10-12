import { forwardRef, useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState } from 'react'
import { DiffEditor, Editor } from '@monaco-editor/react'
import type * as Monaco from 'monaco-editor/editor/editor.api'
import type { ChangedFile, RepoRef, TreeEntry } from '../api'
import { errorMessage } from '../api'
import { SANDY_BEACH_THEME } from '../monaco/theme'
import { LIMITS, isGitlink, shortSha } from '../degrade'
import { formatBytes, loadFile, loadUnchanged, models, prefetchFile, type LoadedFile, type SideInfo } from '../monaco/files'
import { isChanged } from '../tree'
import { ImageDiff, ImageView } from './ImageDiff'

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
  onChangeInfo(info: ChangeInfo | null): void
}

type PaneState =
  | { kind: 'loading' }
  | { kind: 'error'; message: string }
  | Exclude<LoadedFile, { kind: 'text' } | { kind: 'single' }>
  | { kind: 'text' | 'single'; plainReason: string | null }

/** Which editor shows a file: the diff editor, or the plain one for unchanged files. */
type Attached = 'diff' | 'single' | null

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
  { repo, file, provisional = false, nextFile, sideBySide, ignoreTrimWhitespace, pendingJump, headerActions, onChangeInfo },
  ref,
) {
  const editorRef = useRef<DiffEditorT | null>(null)
  const [editorReady, setEditorReady] = useState(false)
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
  const viewStates = useRef(new Map<string, Monaco.editor.IDiffEditorViewState>())
  /** As above, for the plain editor. */
  const singlePath = useRef<string | null>(null)
  const singleViewStates = useRef(new Map<string, Monaco.editor.ICodeEditorViewState>())
  /** Index of the change we last navigated to (valid while the caret stays on it). */
  const navIndex = useRef<number | null>(null)
  /** True until the user moves the caret in a freshly opened file: F7 then goes to change #1 even at line 1. */
  const fresh = useRef(true)
  const onChangeInfoRef = useRef(onChangeInfo)
  onChangeInfoRef.current = onChangeInfo

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
      setEditorReady(true)
    },
    [],
  )
  // The mount callback is registered once; route it to the latest closures.
  const consumeRef = useRef(consumePendingJump)
  consumeRef.current = consumePendingJump
  const reportRef = useRef(reportChangeInfo)
  reportRef.current = reportChangeInfo

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
      if (vs) viewStates.current.set(shownPath.current, vs)
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
            if (vs) viewStates.current.set(prevPath, vs)
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
            const vs = viewStates.current.get(file.path)
            fresh.current = !vs
            if (vs) ed.restoreViewState(vs)
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
      <div className="diff-body">
        {singleWanted && (
          <div className={`diff-editor single-editor${showSingle ? '' : ' is-hidden'}`} data-testid="single-editor">
            <Editor
              theme={SANDY_BEACH_THEME}
              options={EDITOR_OPTIONS}
              keepCurrentModel
              onMount={handleSingleMount}
              loading={<div className="pane-msg">正在加载编辑器…</div>}
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
            loading={<div className="pane-msg">正在加载编辑器…</div>}
          />
        </div>
        {overlay}
      </div>
    </div>
  )
})

function sideSize(s: SideInfo): string {
  return s.bytes === null ? '（无）' : formatBytes(s.bytes) + (s.lines ? `，${s.lines.toLocaleString()} 行` : '')
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
      return <div className="pane-msg pane-loading">加载中…</div>
    case 'error':
      return (
        <div className="pane-msg">
          <div className="pane-error">{state.message}</div>
          <button className="btn" onClick={actions.retry}>重试</button>
        </div>
      )
    case 'binary':
      return (
        <div className="pane-msg">
          <div className="pane-title">二进制文件</div>
          <div className="pane-sub">大小 {sizeLine(file, state.base, state.head)}</div>
        </div>
      )
    case 'deferred':
      return (
        <div className="pane-msg">
          <div className="pane-title">文件过大，未自动加载</div>
          <div className="pane-sub">{sizeLine(file, state.base, state.head)}</div>
          <div className="pane-sub">超过 5 MB 或 5 万行的文件需要手动加载，加载后不做语法高亮。</div>
          <button className="btn btn-primary" onClick={actions.load}>点击加载</button>
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
      <div className="pane-title">子模块</div>
      <div className="pane-sub submodule-line">
        <SubmoduleSide oid={entry.oid} mode={entry.mode} />
      </div>
    </div>
  )
}

function SubmoduleSide({ oid, mode }: { oid: string | null; mode: string }) {
  if (!oid) return <span className="muted">（无）</span>
  return (
    <code title={oid}>
      {isGitlink(mode) ? 'Subproject commit ' : '文件 '}
      {shortSha(oid)}
    </code>
  )
}

/** Gitlink entry: only the recorded commit ids, nothing is fetched (they live in another repository). */
function SubmoduleDiff({ file }: { file: ChangedFile }) {
  return (
    <div className="pane-msg" data-testid="submodule-diff">
      <div className="pane-title">子模块</div>
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
      {(!file.oldOid || !file.newOid) && <div className="pane-sub">{file.oldOid ? '已移除' : '新增'}</div>}
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
        <span className="chip" title="这个文件在 PR 里没有改动，显示的是 head 上的内容">
          未改动
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
        <span className="chip" title="文件模式变更">
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
