// The "+" in the glyph margin that opens a box for a new comment, like github.com's: it shows
// on the line under the mouse when that line takes comments; dragging from it down or up picks
// a range of lines. Monaco leaves clicks in the glyph margin alone (no selection, no focus).
import type * as Monaco from 'monaco-editor/editor/editor.api'

type CodeEditorT = Monaco.editor.ICodeEditor

// `monaco.editor.MouseTargetType`, without loading Monaco here.
const GUTTER_GLYPH_MARGIN = 2
const GUTTER_LINE_NUMBERS = 3
const GUTTER_LINE_DECORATIONS = 4
const CONTENT_TEXT = 6
const CONTENT_EMPTY = 7
/** Targets that stand for a line of code. */
const ON_LINE = new Set([GUTTER_GLYPH_MARGIN, GUTTER_LINE_NUMBERS, GUTTER_LINE_DECORATIONS, CONTENT_TEXT, CONTENT_EMPTY])

export interface PickedLines {
  /** Equal to `line` for one line. */
  startLine: number
  line: number
}

export class CommentGutter {
  private readonly hover: Monaco.editor.IEditorDecorationsCollection
  private readonly picked: Monaco.editor.IEditorDecorationsCollection
  private readonly disposables: Monaco.IDisposable[] = []
  private allowed: ((line: number) => boolean) | null = null
  private hoverLine: number | null = null
  /** The line the "+" shows on now. */
  private shownLine: number | null = null
  private drag: { from: number; to: number; end(): void } | null = null

  constructor(
    ed: CodeEditorT,
    private readonly onPick: (lines: PickedLines) => void,
  ) {
    this.hover = ed.createDecorationsCollection()
    this.picked = ed.createDecorationsCollection()
    this.disposables.push(
      ed.onMouseMove((e) => this.onMove(e)),
      ed.onMouseLeave(() => this.setHover(null)),
      ed.onMouseDown((e) => this.onDown(e)),
      ed.onDidChangeModel(() => this.cancel()),
    )
  }

  /** Which lines take comments; null: none (no "+"). */
  setAllowed(allowed: ((line: number) => boolean) | null): void {
    this.allowed = allowed
    if (this.drag && !(allowed?.(this.drag.from) ?? false)) this.cancel()
    this.setHover(this.hoverLine, true)
  }

  dispose(): void {
    this.cancel()
    this.hover.clear()
    this.shownLine = null
    for (const d of this.disposables) d.dispose()
  }

  private takes(line: number): boolean {
    return this.allowed?.(line) ?? false
  }

  private lineAt(e: Monaco.editor.IEditorMouseEvent): number | null {
    return ON_LINE.has(e.target.type) ? (e.target.position?.lineNumber ?? null) : null
  }

  private onMove(e: Monaco.editor.IEditorMouseEvent): void {
    const line = this.lineAt(e)
    // The button went up where we didn't hear it (another window, a lost pointer).
    if (this.drag && (e.event.buttons & 1) === 0) this.cancel()
    if (this.drag) {
      // A range ends on a line GitHub takes too: past the last one, it stays there.
      if (line !== null && this.takes(line) && line !== this.drag.to) {
        this.drag.to = line
        this.showPicked()
      }
      return
    }
    this.setHover(line)
  }

  private setHover(line: number | null, force = false): void {
    this.hoverLine = line
    const show = line !== null && !this.drag && this.takes(line)
    const shown = show ? line : null
    if (shown === this.shownLine && !force) return
    this.shownLine = shown
    this.hover.set(
      show
        ? [{ range: { startLineNumber: line, startColumn: 1, endLineNumber: line, endColumn: 1 }, options: { glyphMarginClassName: 'tl-add-comment' } }]
        : [],
    )
  }

  private onDown(e: Monaco.editor.IEditorMouseEvent): void {
    if (e.target.type !== GUTTER_GLYPH_MARGIN || !e.event.leftButton || e.event.ctrlKey || e.event.metaKey) return
    const line = e.target.position?.lineNumber
    if (line === undefined || !this.takes(line)) return
    e.event.preventDefault()
    this.cancel()
    const up = (ev: MouseEvent) => {
      if (ev.button !== 0) return
      const d = this.drag
      this.cancel()
      if (d) this.onPick({ startLine: Math.min(d.from, d.to), line: Math.max(d.from, d.to) })
    }
    const key = (ev: KeyboardEvent) => {
      if (ev.key !== 'Escape') return
      ev.preventDefault()
      ev.stopPropagation()
      this.cancel()
    }
    const lost = () => this.cancel()
    // A tap on a touchscreen sends `pointerup` before the `mousedown` that starts this; its
    // `mouseup` comes after. Whichever comes first ends the drag and removes both.
    window.addEventListener('pointerup', up, true)
    window.addEventListener('mouseup', up, true)
    window.addEventListener('pointercancel', lost, true)
    window.addEventListener('blur', lost)
    window.addEventListener('keydown', key, true)
    this.drag = {
      from: line,
      to: line,
      end: () => {
        window.removeEventListener('pointerup', up, true)
        window.removeEventListener('mouseup', up, true)
        window.removeEventListener('pointercancel', lost, true)
        window.removeEventListener('blur', lost)
        window.removeEventListener('keydown', key, true)
      },
    }
    this.hover.clear()
    this.shownLine = null
    this.showPicked()
  }

  private showPicked(): void {
    const d = this.drag
    if (!d) return
    const from = Math.min(d.from, d.to)
    const to = Math.max(d.from, d.to)
    this.picked.set([
      { range: { startLineNumber: from, startColumn: 1, endLineNumber: to, endColumn: 1 }, options: { isWholeLine: true, className: 'tl-line-pick' } },
      { range: { startLineNumber: d.to, startColumn: 1, endLineNumber: d.to, endColumn: 1 }, options: { glyphMarginClassName: 'tl-add-comment is-active' } },
    ])
  }

  private cancel(): void {
    this.drag?.end()
    this.drag = null
    this.picked.clear()
  }
}
