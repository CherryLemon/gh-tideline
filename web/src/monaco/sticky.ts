// The sticky header: the classes and functions around the first visible line of an editor, drawn over its top lines.
// Monaco's own sticky scroll is switched off in the diff editor while it hides unchanged regions, and it reads folding
// ranges; this one reads the server's symbols, so it works on both sides of a diff and in the plain editor alike.
import type * as Monaco from 'monaco-editor/editor/editor.api'
import type { RepoRef, SymbolInfo } from '../api'
import { colorizedRuns, type TextRun } from '../nav/colorized'
import { stickyLines } from '../nav/sticky'
import { getSymbols } from '../nav/symbols'
import { parseModelUri } from './files'
import { monaco } from './setup'

type EditorT = Monaco.editor.ICodeEditor

/** A line longer than this is cut and left uncolored: the header holds names, and a minified file is one huge line. */
const MAX_ROW_CHARS = 1000

export interface StickyHost {
  repo(): RepoRef
  /** A row was clicked: take the editor to `line`. */
  reveal(line: number): void
}

/** The runs of one line of `model`, colored the way the editor colors it; plain text if Monaco has no colors for it. */
function lineRuns(model: Monaco.editor.ITextModel, line: number): TextRun[] {
  const content = model.getLineContent(line)
  if (content.length <= MAX_ROW_CHARS) {
    try {
      const runs = colorizedRuns(monaco.editor.colorizeModelLine(model, line, model.getOptions().tabSize))
      if (runs.length > 0) return runs
    } catch {
      // Plain text below.
    }
  }
  return [{ text: content.slice(0, MAX_ROW_CHARS), className: '' }]
}

/**
 * One editor's header, an overlay widget at its top. While `setActive(true)`, it loads the symbols of the model shown and
 * keeps one row per scope around the editor's first visible line, each with its line number and its text from column 1.
 */
export class StickyHeader {
  private readonly node = document.createElement('div')
  private readonly widget: Monaco.editor.IOverlayWidget
  private readonly listeners: Monaco.IDisposable[]
  private active = false
  /** The URI of the model `symbols` are for; null while none are (or are wanted). */
  private forModel: string | null = null
  private symbols: readonly SymbolInfo[] = []
  /** The lines shown in the rows. */
  private lines: number[] = []

  constructor(
    private readonly editor: EditorT,
    private readonly host: StickyHost,
  ) {
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
      this.editor.focus()
      this.host.reveal(Number(row.dataset.line))
    })
    node.addEventListener('wheel', (e) => this.onWheel(e), { passive: false })
    const position = { preference: { top: 0, left: 0 } }
    this.widget = { getId: () => 'tl-sticky', getDomNode: () => node, getPosition: () => position }
    editor.addOverlayWidget(this.widget)
    this.listeners = [
      editor.onDidScrollChange((e) => e.scrollTopChanged && this.update()),
      // The editor scrolls to the caret only after this event, so the view is looked at once the key or the call is done.
      editor.onDidChangeCursorPosition((e) => {
        const line = e.position.lineNumber
        if (e.source === 'keyboard' || e.source === 'api') queueMicrotask(() => this.keepCaretClear(line))
      }),
      // Cards and unfolded lines move the lines without scrolling.
      editor.onDidContentSizeChange(() => this.update()),
      editor.onDidChangeHiddenAreas(() => this.update()),
      editor.onDidLayoutChange(() => this.layout()),
      editor.onDidChangeConfiguration((e) => {
        if (e.hasChanged(monaco.editor.EditorOption.fontInfo) || e.hasChanged(monaco.editor.EditorOption.lineHeight)) this.layout()
      }),
      editor.onDidChangeModel(() => this.load()),
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
    this.listeners.forEach((l) => l.dispose())
    this.editor.removeOverlayWidget(this.widget)
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
  }

  /** Asks for the symbols of the model shown, unless the header is off or the model is not a file with a grammar. */
  private load(): void {
    const model = this.active ? this.editor.getModel() : null
    const target = model && model.getLanguageId() !== 'plaintext' ? parseModelUri(model.uri) : null
    const key = model && target?.oid ? model.uri.toString() : null
    if (key === this.forModel) return
    this.forModel = key
    this.symbols = []
    this.update()
    if (!target?.oid) return
    getSymbols(this.host.repo(), target.oid, target.path).then(
      (r) => {
        // Not the model any more, or no longer wanted.
        if (this.forModel !== key) return
        this.symbols = r.symbols
        this.update()
      },
      () => undefined,
    )
  }

  /** Redraws the rows if they are not the ones the first visible line calls for. */
  private update(): void {
    const model = this.editor.getModel()
    const top = model && this.symbols.length > 0 ? this.editor.getVisibleRanges()[0]?.startLineNumber : undefined
    const lines = top === undefined ? [] : stickyLines(this.symbols, top).map((s) => s.line)
    if (lines.length === this.lines.length && lines.every((l, i) => l === this.lines[i])) return
    this.lines = lines
    this.node.replaceChildren(...(model ? lines.map((line) => this.row(model, line)) : []))
    this.node.hidden = lines.length === 0
  }

  /**
   * Monaco does not know the rows cover the first lines: a caret that a key or a call moved to the top of the view lands under
   * them. The view is moved up until the caret is just below the rows, which grow as it moves (they are four at most).
   */
  private keepCaretClear(line: number): void {
    if (!this.active) return
    const editor = this.editor
    const rowHeight = editor.getOption(monaco.editor.EditorOption.lineHeight)
    const top = editor.getTopForLineNumber(line)
    for (let round = 0; round < 4; round++) {
      this.update()
      const covered = this.lines.length * rowHeight
      if (top < editor.getScrollTop() || top >= editor.getScrollTop() + covered) return
      editor.setScrollTop(top - covered, monaco.editor.ScrollType.Immediate)
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
    // Text nodes only: repository text is never parsed as markup (design.md §8).
    for (const run of lineRuns(model, line)) {
      const span = document.createElement('span')
      span.className = run.className
      span.textContent = run.text
      text.appendChild(span)
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
