import { forwardRef, useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState } from 'react'
import { DiffEditor } from '@monaco-editor/react'
import type * as Monaco from 'monaco-editor/editor/editor.api'
import type { ChangedFile, RepoRef } from '../api'
import { errorMessage } from '../api'
import { SANDY_BEACH_THEME } from '../monaco/theme'
import { LIMITS, isGitlink, shortSha } from '../degrade'
import { formatBytes, loadFile, models, prefetchFile, type LoadedFile, type SideInfo } from '../monaco/files'
import { ImageDiff } from './ImageDiff'

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
  file: ChangedFile
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
  | Exclude<LoadedFile, { kind: 'text' }>
  | { kind: 'text'; plainReason: string | null }

/** Diff limits for the attached file; a force-loaded file has none. */
function diffLimits(forced: boolean): Pick<Monaco.editor.IDiffEditorOptions, 'maxComputationTime' | 'maxFileSize'> {
  return {
    maxComputationTime: forced ? 0 : LIMITS.maxComputationTimeMs,
    maxFileSize: forced ? 0 : LIMITS.maxFileSizeMb,
  }
}

type DiffEditorT = Monaco.editor.IStandaloneDiffEditor

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
  { repo, file, nextFile, sideBySide, ignoreTrimWhitespace, pendingJump, headerActions, onChangeInfo },
  ref,
) {
  const editorRef = useRef<DiffEditorT | null>(null)
  const [editorReady, setEditorReady] = useState(false)
  /** Load state tagged with its file: until the load effect has run for a new file, it is loading. */
  const [loaded, setLoaded] = useState<{ path: string | null; state: PaneState }>({ path: null, state: LOADING })
  const state = loaded.path === file.path ? loaded.state : LOADING
  const [forced, setForced] = useState<ReadonlySet<string>>(() => new Set())
  const [retry, setRetry] = useState(0)
  /** Whether some file's models are attached; while the next file loads, its content stays visible. */
  const [attached, setAttached] = useState(false)
  /** Whether the attached file was force-loaded; part of `options` so re-applied options keep its limits. */
  const [attachedForced, setAttachedForced] = useState(false)

  /** Path whose models are currently attached to the editor. */
  const shownPath = useRef<string | null>(null)
  const viewStates = useRef(new Map<string, Monaco.editor.IDiffEditorViewState>())
  /** Index of the change we last navigated to (valid while the caret stays on it). */
  const navIndex = useRef<number | null>(null)
  /** True until the user moves the caret in a freshly opened file: F7 then goes to change #1 even at line 1. */
  const fresh = useRef(true)
  const onChangeInfoRef = useRef(onChangeInfo)
  onChangeInfoRef.current = onChangeInfo

  const options = useMemo<Monaco.editor.IDiffEditorConstructionOptions>(
    () => ({
      readOnly: true,
      domReadOnly: true,
      originalEditable: false,
      renderSideBySide: sideBySide,
      useInlineViewWhenSpaceIsLimited: false,
      ignoreTrimWhitespace,
      hideUnchangedRegions: { enabled: true, contextLineCount: 3, minimumLineCount: 3, revealLineCount: 20 },
      diffAlgorithm: 'advanced',
      experimental: { showMoves: true },
      renderOverviewRuler: true,
      ...diffLimits(attachedForced),
      fontFamily: "'JetBrains Mono', ui-monospace, monospace",
      fontSize: 13,
      lineHeight: 20,
      fontLigatures: false,
      minimap: { enabled: false },
      scrollBeyondLastLine: false,
      stickyScroll: { enabled: false },
      renderLineHighlight: 'line',
      renderMarginRevertIcon: false,
      renderGutterMenu: false,
      automaticLayout: true,
      scrollbar: { verticalScrollbarSize: 12, horizontalScrollbarSize: 12, useShadows: false },
      padding: { top: 4 },
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
      if (state.kind !== 'loading' && state.kind !== 'text') return 'none'
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
    [activeSide, file.path, jumpTo, pendingJump, state.kind],
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

  const detach = useCallback(() => {
    const ed = editorRef.current
    if (!ed) return
    if (shownPath.current !== null && ed.getModel()) {
      const vs = ed.saveViewState()
      if (vs) viewStates.current.set(shownPath.current, vs)
    }
    shownPath.current = null
    ed.setModel(null)
    models.setInUse([])
    setAttached(false)
  }, [])

  const isForced = forced.has(file.path)

  useEffect(() => {
    if (!editorReady) return
    let cancelled = false
    let imageUrls: string[] = []
    const setState = (st: PaneState) => setLoaded({ path: file.path, state: st })
    setState(LOADING)
    onChangeInfoRef.current(null)
    loadFile(repo, file, isForced)
      .then((res) => {
        if (res.kind === 'image') imageUrls = [res.baseUrl, res.headUrl].filter((u): u is string => !!u)
        if (cancelled) {
          imageUrls.forEach((u) => URL.revokeObjectURL(u))
          if (res.kind === 'text') res.release()
          return
        }
        const ed = editorRef.current!
        if (res.kind !== 'text') {
          detach()
          if (pendingJump.current?.path === file.path) pendingJump.current = null
          setState(res)
        } else {
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
          setAttached(true)
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
  }, [editorReady, repo, file, isForced, retry])

  const showEditor = state.kind === 'text' || (state.kind === 'loading' && attached)
  const overlay = renderOverlay(state, file, {
    load: () => setForced((s) => new Set(s).add(file.path)),
    retry: () => setRetry((n) => n + 1),
  })

  return (
    <div className="diff-pane">
      <FileHeader file={file} plainReason={state.kind === 'text' ? state.plainReason : null} actions={headerActions} />
      <div className="diff-body">
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

function sizeLine(base: SideInfo, head: SideInfo): string {
  const fmt = (s: SideInfo) => (s.bytes === null ? '（无）' : formatBytes(s.bytes) + (s.lines ? `，${s.lines.toLocaleString()} 行` : ''))
  return `${fmt(base)} → ${fmt(head)}`
}

function renderOverlay(state: PaneState, file: ChangedFile, actions: { load(): void; retry(): void }) {
  switch (state.kind) {
    case 'text':
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
          <div className="pane-sub">大小 {sizeLine(state.base, state.head)}</div>
        </div>
      )
    case 'deferred':
      return (
        <div className="pane-msg">
          <div className="pane-title">文件过大，未自动加载</div>
          <div className="pane-sub">{sizeLine(state.base, state.head)}</div>
          <div className="pane-sub">超过 5 MB 或 5 万行的文件需要手动加载，加载后不做语法高亮。</div>
          <button className="btn btn-primary" onClick={actions.load}>点击加载</button>
        </div>
      )
    case 'image':
      return <ImageDiff key={file.path} file={file} state={state} />
    case 'submodule':
      return <SubmoduleDiff file={file} />
  }
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

function FileHeader({ file, plainReason, actions }: { file: ChangedFile; plainReason: string | null; actions?: React.ReactNode }) {
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
