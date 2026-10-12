import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { getPr, getPrFiles, errorMessage, type ChangedFile, type PrSummary, type RepoRef } from '../api'
import { fileQuery, navigate, type PrRoute } from '../router'
import { GENERATED_KEY, ancestorKeys, buildTree } from '../tree'
import { usePersistentState } from '../storage'
import { DiffPane, type ChangeInfo, type DiffPaneHandle, type PendingJump } from './DiffPane'
import { FileTree } from './FileTree'
import { FileFinder } from './FileFinder'

interface Props {
  route: PrRoute
  search: string
}

type Data = { pr: PrSummary; files: ChangedFile[] }

const DOUBLE_SHIFT_MS = 350

export function PrPage({ route, search }: Props) {
  const repo = useMemo<RepoRef>(
    () => ({ host: route.host, owner: route.owner, repo: route.repo }),
    [route.host, route.owner, route.repo],
  )
  const [data, setData] = useState<Data | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [refreshing, setRefreshing] = useState(false)

  const load = useCallback(
    async (refresh: boolean) => {
      const [pr, files] = await Promise.all([getPr(repo, route.number, refresh), getPrFiles(repo, route.number, refresh)])
      return { pr, files }
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

  const refresh = () => {
    setRefreshing(true)
    load(true)
      .then(setData, (e: unknown) => setError(errorMessage(e)))
      .finally(() => setRefreshing(false))
  }

  useEffect(() => {
    if (data) document.title = `#${data.pr.number} ${data.pr.title} · ${data.pr.owner}/${data.pr.repo}`
  }, [data])

  if (error) {
    return (
      <div className="page-message">
        <div className="page-message-box">
          <div className="page-message-title">无法打开 PR</div>
          <div className="page-message-body">{error}</div>
          <a className="btn" href="/">返回首页</a>
        </div>
      </div>
    )
  }
  if (!data) {
    return (
      <div className="page-message">
        <div className="muted">正在打开 {route.owner}/{route.repo} #{route.number}…</div>
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
  const { pr, files } = data
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set([GENERATED_KEY]))
  const tree = useMemo(() => buildTree(files, collapsed as Set<string>), [files, collapsed])
  const order = tree.order

  const [sideBySide, setSideBySide] = usePersistentState('dr.sideBySide', true)
  const [ignoreWs, setIgnoreWs] = usePersistentState('dr.ignoreTrimWhitespace', false)
  const [finderOpen, setFinderOpen] = useState(false)
  const [changeInfo, setChangeInfo] = useState<ChangeInfo | null>(null)
  const [treeWidth, setTreeWidth] = usePersistentState('dr.treeWidth', 320)

  const pendingJump = useRef<PendingJump | null>(null)
  const pane = useRef<DiffPaneHandle>(null)

  const requested = new URLSearchParams(search).get('file')
  const currentIndex = order.findIndex((f) => f.path === requested)
  const current = currentIndex >= 0 ? order[currentIndex] : null

  // Without a valid ?file=, show the first file (replace, so Back leaves the PR).
  useEffect(() => {
    if (!current && order.length > 0) navigate(location.pathname + fileQuery(order[0].path, location.search), true)
  }, [current, order])

  // Reveal the current file in the tree when it changes; later collapsing its folder sticks.
  const currentPath = current?.path ?? null
  const currentGenerated = current?.generated ?? false
  useEffect(() => {
    if (currentPath === null) return
    const keys = ancestorKeys({ path: currentPath, generated: currentGenerated })
    setCollapsed((prev) => {
      if (!keys.some((k) => prev.has(k))) return prev
      const next = new Set(prev)
      keys.forEach((k) => next.delete(k))
      return next
    })
  }, [currentPath, currentGenerated])

  const openFile = useCallback(
    (path: string, jump?: PendingJump['where']) => {
      pendingJump.current = jump ? { path, where: jump } : null
      if (path !== requested) navigate(location.pathname + fileQuery(path, location.search))
    },
    [requested],
  )

  const toggle = useCallback((key: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }, [])

  // Keep the latest navigation state for the window-level key handler.
  const nav = useRef({ order, currentIndex, openFile, finderOpen })
  nav.current = { order, currentIndex, openFile, finderOpen }

  useEffect(() => {
    let lastShift = 0
    const onKeyDown = (e: KeyboardEvent) => {
      const { order, currentIndex, openFile, finderOpen } = nav.current
      if (e.key === 'Shift') {
        if (e.repeat) return
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
      if (e.key === 'F7' && plain) {
        // Captured on window before Monaco sees it: overrides its accessible-diff-viewer binding.
        consume()
        // 'notReady' (file or diff still loading) must not advance: the pane queues the jump instead.
        if (e.shiftKey) {
          const r = pane.current?.prevChange() ?? 'none'
          if (r === 'none' && currentIndex > 0) openFile(order[currentIndex - 1].path, 'last')
        } else {
          const r = pane.current?.nextChange() ?? 'none'
          if (r === 'none' && currentIndex >= 0 && currentIndex < order.length - 1) openFile(order[currentIndex + 1].path, 'first')
        }
        return
      }
      if (e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
        consume()
        const target = currentIndex + (e.key === 'ArrowDown' ? 1 : -1)
        if (target >= 0 && target < order.length) openFile(order[target].path)
        return
      }
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 'p') {
        consume()
        setFinderOpen(true)
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

  const nextFile = currentIndex >= 0 && currentIndex < order.length - 1 ? order[currentIndex + 1] : null

  return (
    <div className="app" style={{ '--tree-width': `${treeWidth}px` } as React.CSSProperties}>
      <header className="topbar">
        <a className="home-link" href="/" title="首页">
          Tideline
        </a>
        <span className="sep" />
        <span className="pr-repo">
          {pr.owner}/{pr.repo}
        </span>
        <a className="pr-number" href={/^https?:\/\//i.test(pr.url) ? pr.url : undefined} target="_blank" rel="noreferrer" title="在 GitHub 上打开">
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
        <div className="segmented" role="group" aria-label="视图模式">
          <button className={sideBySide ? 'is-on' : ''} onClick={() => setSideBySide(true)} aria-pressed={sideBySide}>
            Split
          </button>
          <button className={!sideBySide ? 'is-on' : ''} onClick={() => setSideBySide(false)} aria-pressed={!sideBySide}>
            Unified
          </button>
        </div>
        <label className="check" title="Monaco 只支持忽略行首行尾的空白">
          <input type="checkbox" checked={ignoreWs} onChange={(e) => setIgnoreWs(e.target.checked)} />
          忽略首尾空白
        </label>
        <button className="btn btn-icon" onClick={onRefresh} disabled={refreshing} title="重新从 GitHub 拉取">
          {refreshing ? '刷新中…' : '刷新'}
        </button>
      </header>

      <div className="main">
        <aside className="sidebar">
          <FileTree rows={tree.rows} files={files} currentPath={current?.path ?? null} onOpen={openFile} onToggle={toggle} />
        </aside>
        <div className="splitter" onMouseDown={startResize} />
        <section className="content">
          {current ? (
            <DiffPane
              ref={pane}
              repo={repo}
              file={current}
              nextFile={nextFile}
              sideBySide={sideBySide}
              ignoreTrimWhitespace={ignoreWs}
              pendingJump={pendingJump}
              onChangeInfo={setChangeInfo}
            />
          ) : (
            <div className="pane-msg muted">{files.length === 0 ? '这个 PR 没有改动文件' : ''}</div>
          )}
        </section>
      </div>

      <footer className="statusbar">
        <span className="status-item" data-testid="change-info">
          {changeInfo
            ? changeInfo.total === 0
              ? '无改动'
              : `改动 ${changeInfo.index === null ? '–' : changeInfo.index + 1}/${changeInfo.total}`
            : '改动 –'}
        </span>
        <span className="status-item">
          文件 {currentIndex >= 0 ? currentIndex + 1 : '–'}/{order.length}
        </span>
        <span className="spacer" />
        <span className="hint"><kbd>F7</kbd> 下一处改动</span>
        <span className="hint"><kbd>Shift+F7</kbd> 上一处</span>
        <span className="hint"><kbd>Alt+↓</kbd>/<kbd>Alt+↑</kbd> 下/上一个文件</span>
        <span className="hint"><kbd>Shift Shift</kbd>/<kbd>Ctrl+P</kbd> 查找文件</span>
      </footer>

      {finderOpen && (
        <FileFinder
          files={order}
          onClose={() => setFinderOpen(false)}
          onPick={(path) => {
            setFinderOpen(false)
            openFile(path)
          }}
        />
      )}
    </div>
  )
}
