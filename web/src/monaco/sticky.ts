// The sticky header: the classes and functions around the first visible line of an editor, drawn over its top lines.
// Monaco's own sticky scroll is switched off in the diff editor while it hides unchanged regions, and it reads folding
// ranges; this one reads the server's symbols, so it works on both sides of a diff and in the plain editor alike.
import type * as Monaco from 'monaco-editor/editor/editor.api'
import type { RepoRef, SymbolInfo } from '../api'
import { colorizedRuns, type TextRun } from '../nav/colorized'
import { lineBelow, type LineEdges, nextBar, rowsBeforeBar, sameLines, scrollToUncover, stickyLines } from '../nav/sticky'
import { getSymbols } from '../nav/symbols'
import { parseModelUri } from './files'
import { monaco } from './setup'

type EditorT = Monaco.editor.ICodeEditor

/** A line longer than this is cut and left uncolored: the header holds names, and a minified file is one huge line. */
const MAX_ROW_CHARS = 1000
/**
 * Monaco draws the handle for dragging the top edge of a bar of unchanged lines this many pixels above the view zone it is in.
 * In monaco-editor 0.57 the bar is a 24px zone after the line before the hidden ones (vs/editor/browser/widget/diffEditor/features/
 * hideUnchangedRegionsFeature.js ~137), and `.diff-hidden-lines` is moved 10px up with its `.top` handle 4px back down (diffEditor/
 * style.css ~12 and ~54). Check them, and the hidden lines of `LineEdges`, on an upgrade.
 */
const BAR_REACH = 6
/** Waits before asking again for symbols that could not be fetched (a server that was busy or restarting); then it gives up. */
const RETRY_MS = [2000, 8000]

export interface StickyHost {
  repo(): RepoRef
  /** A row was clicked: take the editor to `line`. False if it is not showing the file yet (it has the previous one), and nothing was done. */
  reveal(line: number): boolean
}

/** Puts `runs` in `el` as text nodes only: repository text is never parsed as markup (design.md §8). */
function setRuns(el: HTMLElement, runs: readonly TextRun[]): void {
  el.replaceChildren(
    ...runs.map((run) => {
      const span = document.createElement('span')
      span.className = run.className
      span.textContent = run.text
      return span
    }),
  )
}

/**
 * One editor's header, an overlay widget at its top. While `setActive(true)`, it loads the symbols of the model shown and
 * keeps one row per scope around the first line the rows leave in view, each with its line number and its text from column 1.
 */
export class StickyHeader {
  private readonly node = document.createElement('div')
  private readonly widget: Monaco.editor.IOverlayWidget
  private readonly listeners: Monaco.IDisposable[]
  private readonly edges: LineEdges
  private active = false
  /** The URI of the model `symbols` are for; null while none are (or are wanted). */
  private forModel: string | null = null
  private symbols: readonly SymbolInfo[] = []
  /** The lines shown in the rows, and the rows: one element per line, kept while the line stays in the header. */
  private lines: number[] = []
  private rows = new Map<number, HTMLElement>()
  /** Where the caret was when `keepCaretClear` last looked. */
  private caret: Monaco.IPosition | null = null
  /** What the rule under the rows adds to the height of the header. */
  private border = 0
  /** Counts the models whose symbols were wanted (and the disposal): an answer to an earlier request is not for the one shown. */
  private generation = 0
  /** How many requests for the symbols of the model have failed, and the wait before the next one. */
  private failures = 0
  private retry: ReturnType<typeof setTimeout> | undefined

  constructor(
    private readonly editor: EditorT,
    private readonly host: StickyHost,
  ) {
    this.edges = {
      top: (line) => editor.getTopForLineNumber(line),
      zonesTop: (line) => editor.getTopForLineNumber(line, true),
      bottom: (line) => editor.getBottomForLineNumber(line),
    }
    const node = this.node
    node.className = 'tl-sticky'
    node.hidden = true
    // The same lines are in the editor, for a screen reader.
    node.setAttribute('aria-hidden', 'true')
    // The header is not text to select or a place for the caret: a press must not take the focus from the editor.
    node.addEventListener('mousedown', (e) => e.preventDefault())
    node.addEventListener('click', (e) => {
      const row = e.target instanceof Element ? e.target.closest<HTMLElement>('.tl-sticky-row') : null
      if (!row) return
      // The press did not move the focus (above); the click does, as one on the code would, also from a box or a field.
      // Not while the editor still shows the previous file: it is not where the user goes.
      if (!this.host.reveal(Number(row.dataset.line))) return
      this.editor.focus()
      // A caret that is on the line already does not move, and Monaco has no reason to scroll: it may be under the rows.
      queueMicrotask(() => this.keepCaretClear(true))
    })
    node.addEventListener('wheel', (e) => this.onWheel(e), { passive: false })
    const position = { preference: { top: 0, left: 0 } }
    this.widget = { getId: () => 'tl-sticky', getDomNode: () => node, getPosition: () => position }
    editor.addOverlayWidget(this.widget)
    this.listeners = [
      editor.onDidScrollChange((e) => e.scrollTopChanged && this.update()),
      // The editor scrolls to the caret only after this event, so the view is looked at once the key or the call is done,
      // and the caret where it is left: a call can move it elsewhere and back (the cards do, to unfold the lines they are on).
      editor.onDidChangeCursorPosition((e) => {
        if (e.source === 'keyboard' || e.source === 'api') queueMicrotask(() => this.keepCaretClear(e.source === 'keyboard'))
        else this.caret = e.position
      }),
      // Cards and unfolded lines move the lines without scrolling.
      editor.onDidContentSizeChange(() => this.update()),
      editor.onDidChangeHiddenAreas(() => this.update()),
      editor.onDidLayoutChange(() => {
        this.layout()
        this.update()
      }),
      editor.onDidChangeConfiguration((e) => {
        if (!e.hasChanged(monaco.editor.EditorOption.fontInfo) && !e.hasChanged(monaco.editor.EditorOption.lineHeight)) return
        this.layout()
        this.update()
      }),
      editor.onDidChangeModel(() => {
        this.caret = null
        this.load()
      }),
      editor.onDidChangeModelLanguage(() => this.load()),
    ]
  }

  /** Whether the header is wanted now: the editor is shown, and the user has not turned the header off. */
  setActive(active: boolean): void {
    if (active === this.active) return
    this.active = active
    if (active) this.layout()
    this.load()
  }

  dispose(): void {
    this.active = false
    this.forModel = null
    this.generation++
    clearTimeout(this.retry)
    this.listeners.forEach((l) => l.dispose())
    this.editor.removeOverlayWidget(this.widget)
  }

  /** How far down the header reaches over the code, in pixels (0 while it shows nothing). */
  height(): number {
    return this.lines.length === 0 ? 0 : this.node.offsetHeight
  }

  /** Where the rows go: over the gutter and the code, not the scrollbar, in the editor's font. */
  private layout(): void {
    const info = this.editor.getLayoutInfo()
    const style = this.node.style
    style.width = `${info.width - info.verticalScrollbarWidth}px`
    style.setProperty('--tl-sticky-line', `${this.editor.getOption(monaco.editor.EditorOption.lineHeight)}px`)
    style.setProperty('--tl-sticky-num', `${info.lineNumbersLeft + info.lineNumbersWidth}px`)
    style.setProperty('--tl-sticky-text', `${info.contentLeft}px`)
    this.editor.applyFontInfo(this.node)
    this.border = parseFloat(getComputedStyle(this.node).borderBottomWidth) || 0
  }

  /** Asks for the symbols of the model shown, unless the header is off or the model is not a file with a grammar. */
  private load(): void {
    const model = this.active ? this.editor.getModel() : null
    const target = model && model.getLanguageId() !== 'plaintext' ? parseModelUri(model.uri) : null
    const key = model && target?.oid ? model.uri.toString() : null
    if (key === this.forModel) return
    this.forModel = key
    this.generation++
    this.symbols = []
    this.failures = 0
    clearTimeout(this.retry)
    this.update()
    if (key !== null && target?.oid) this.fetch(target.oid, target.path)
  }

  /** Gets the symbols of the model shown; if that fails, asks again after a wait, a few times. */
  private fetch(oid: string, path: string): void {
    const generation = this.generation
    getSymbols(this.host.repo(), oid, path).then(
      (r) => {
        const model = this.editor.getModel()
        // Not the model any more, or no longer wanted.
        if (generation !== this.generation || !model) return
        // A name on a line the model does not have has no row to show.
        const count = model.getLineCount()
        this.symbols = r.symbols.filter((s) => s.line >= 1 && s.line <= count)
        this.update()
      },
      () => {
        if (generation !== this.generation || this.failures >= RETRY_MS.length) return
        clearTimeout(this.retry)
        this.retry = setTimeout(() => this.fetch(oid, path), RETRY_MS[this.failures++])
      },
    )
  }

  /** The lines of the scopes to pin now, outermost first. */
  private scopeLines(): number[] {
    const editor = this.editor
    const ranges = editor.getVisibleRanges()
    const scrollTop = editor.getScrollTop()
    const rowHeight = editor.getOption(monaco.editor.EditorOption.lineHeight)
    // The rows would cover the controls of a bar of unchanged lines (and the handle that drags its top edge, which is drawn above
    // it): they stay above it, as many as fit (none once it reaches the top, which the one over the first change does at once),
    // and come whole when it has scrolled out of view.
    const bar = nextBar(ranges, this.edges, scrollTop, editor.getOption(monaco.editor.EditorOption.padding).top)
    const maxRows = rowsBeforeBar(bar, scrollTop, rowHeight, this.border + BAR_REACH)
    return stickyLines(this.symbols, (rows) => lineBelow(ranges, this.edges.bottom, scrollTop + rows * rowHeight), maxRows).map((s) => s.line)
  }

  /** Redraws the rows if they are not the ones the lines in view call for. */
  private update(): void {
    const model = this.editor.getModel()
    const lines = model && this.symbols.length > 0 ? this.scopeLines() : []
    if (sameLines(lines, this.lines)) return
    this.lines = lines
    const rows = new Map<number, HTMLElement>()
    if (model) for (const line of lines) rows.set(line, this.rows.get(line) ?? this.row(model, line))
    this.rows = rows
    this.node.replaceChildren(...rows.values())
    this.node.hidden = lines.length === 0
  }

  /**
   * Monaco does not know the rows cover the first lines: a caret that a key or a call moved to the top of the view lands under
   * them. The view is moved up until the caret is just below the rows, which grow as it moves (they are four at most).
   * `always`: also if the caret stayed where it was (a key that goes along the line), as the user is at it; a call counts
   * only if it left the caret somewhere else, since the cards move it away and back to unfold the lines they are on.
   */
  private keepCaretClear(always: boolean): void {
    const caret = this.editor.getPosition()
    if (!caret) return
    const moved = !monaco.Position.equals(caret, this.caret)
    this.caret = caret
    if (!this.active || !(always || moved)) return
    const editor = this.editor
    const top = editor.getTopForLineNumber(caret.lineNumber)
    for (let round = 0; round < 4; round++) {
      this.update()
      const next = scrollToUncover(top, editor.getScrollTop(), this.height())
      if (next === null) return
      editor.setScrollTop(next, monaco.editor.ScrollType.Immediate)
    }
  }

  private row(model: Monaco.editor.ITextModel, line: number): HTMLElement {
    const row = document.createElement('div')
    row.className = 'tl-sticky-row'
    row.dataset.line = String(line)
    const num = document.createElement('span')
    num.className = 'tl-sticky-num'
    num.textContent = String(line)
    const text = document.createElement('span')
    text.className = 'tl-sticky-text'
    const content = model.getLineContent(line)
    const tabSize = model.getOptions().tabSize
    // Plain text first, tabs as wide as the editor shows them; the colors follow when Monaco has them. A line this long, a
    // minified file's, stays plain, cut.
    text.style.tabSize = String(tabSize)
    setRuns(text, [{ text: content.slice(0, MAX_ROW_CHARS), className: '' }])
    if (content.length <= MAX_ROW_CHARS) {
      monaco.editor
        .colorize(content, model.getLanguageId(), { tabSize })
        .then((html) => {
          const runs = colorizedRuns(html)
          if (runs.length > 0) setRuns(text, runs)
        })
        .catch(() => undefined)
    }
    row.append(num, text)
    return row
  }

  /** The wheel scrolls the code under the header. */
  private onWheel(e: WheelEvent): void {
    const delegate = (this.editor as EditorT & { delegateScrollFromMouseWheelEvent?(e: WheelEvent): void }).delegateScrollFromMouseWheelEvent
    if (e.ctrlKey || typeof delegate !== 'function') return
    // Monaco skips wheel events whose default is prevented, so prevent it only afterwards.
    delegate.call(this.editor, e)
    e.preventDefault()
  }
}
