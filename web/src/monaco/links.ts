// Names as links: with Ctrl (Cmd on a Mac) held, the name under the pointer is underlined and clicking it asks for its
// definition. Only the code takes part; the rows of the sticky header, the comment cards and the gutters are not names.
import type * as Monaco from 'monaco-editor/editor/editor.api'
import { clickOn, type Click } from '../nav/choose'
import { identifierAt, type IdentifierSource, type TokenKind } from '../nav/identifier'
import { parseModelUri } from './files'

type EditorT = Monaco.editor.ICodeEditor

// `monaco.editor.MouseTargetType.CONTENT_TEXT`, without loading Monaco here.
const CONTENT_TEXT = 6

const MAC = /Mac/.test(navigator.userAgent)

/** Whether the key that turns names into links is down in an event (Ctrl; on a Mac, where Ctrl+click is a right click, Cmd). */
const linkKeyDown = (e: { ctrlKey: boolean; metaKey: boolean }) => (MAC ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey)

/** A name in an editor, and where it stands: what a lookup of its definitions starts from. */
export interface NavSource {
  /** The name clicked, in the file as the commit of its side has it (a renamed file has its old path on the base side). */
  click: Click
  /** The blob shown, which the file's symbols are read from; null for a side the file doesn't have. */
  oid: string | null
  /** Where a list opened for the name goes, in window coordinates: at the pointer, or at the caret. */
  anchor: { x: number; y: number }
}

/** Monaco's `StandardTokenType`, in order. */
const TOKEN_KINDS: TokenKind[] = ['code', 'comment', 'string', 'regex']

/** What a text model has for reading its tokens, that Monaco's typings leave out; the editor itself reads the same. */
interface Tokenization {
  isCheapToTokenize(line: number): boolean
  forceTokenization(line: number): void
  getLineTokens(line: number): { findTokenIndexAtOffset(offset: number): number; getStandardTokenType(index: number): number }
}

function tokenKindAt(model: Monaco.editor.ITextModel, line: number, column: number): TokenKind | null {
  const t = (model as Monaco.editor.ITextModel & { tokenization?: Partial<Tokenization> }).tokenization
  if (!t?.isCheapToTokenize || !t.forceTokenization || !t.getLineTokens) return null
  try {
    // A line far down a file nobody has scrolled to would take the tokenizer through all the lines before it.
    if (!t.isCheapToTokenize(line)) return null
    t.forceTokenization(line)
    const tokens = t.getLineTokens(line)
    return TOKEN_KINDS[tokens.getStandardTokenType(tokens.findTokenIndexAtOffset(column - 1))] ?? null
  } catch {
    return null
  }
}

function modelSource(model: Monaco.editor.ITextModel): IdentifierSource {
  return {
    getLanguageId: () => model.getLanguageId(),
    getLineContent: (line) => model.getLineContent(line),
    tokenKindAt: (line, column) => tokenKindAt(model, line, column),
  }
}

/** The name at `position` in `editor`'s model, as the source of a lookup; null if there is none, or the model isn't one of ours. */
export function sourceAt(editor: EditorT, position: Monaco.IPosition, anchor: NavSource['anchor']): NavSource | null {
  const model = editor.getModel()
  const version = model ? parseModelUri(model.uri) : null
  const id = model && version ? identifierAt(modelSource(model), position) : null
  return id && version ? { click: clickOn(version.path, version.side, id), oid: version.oid, anchor } : null
}

/** Where the caret is on screen (below its line), in window coordinates, kept inside the editor; its corner when the caret is out of view. */
export function caretAnchor(editor: EditorT): NavSource['anchor'] {
  const box = editor.getDomNode()?.getBoundingClientRect()
  if (!box) return { x: 0, y: 0 }
  const caret = editor.getPosition()
  const at = caret ? editor.getScrolledVisiblePosition(caret) : null
  const x = box.left + (at ? at.left : 0)
  const y = box.top + (at ? at.top + at.height : 0)
  return { x: Math.min(Math.max(x, box.left), box.right), y: Math.min(Math.max(y, box.top), box.bottom) }
}

/**
 * Ctrl+click on a name in `editor` calls `onActivate`. The click is the press and the release on one name, so that
 * dragging a selection with Ctrl held asks for nothing.
 */
export class NavLinks {
  private readonly underline: Monaco.editor.IEditorDecorationsCollection
  private readonly listeners: Monaco.IDisposable[]
  private readonly onKey = (e: KeyboardEvent) => this.setHeld(linkKeyDown(e))
  private readonly onBlur = () => this.setHeld(false)
  private held = false
  /** Where the pointer is, when it is over text. */
  private over: Monaco.IPosition | null = null
  /** The name pressed on with the key down, until the button is released. */
  private pressed: NavSource | null = null
  /** The name underlined now, as `line:column`. */
  private shown: string | null = null

  constructor(
    private readonly editor: EditorT,
    private readonly onActivate: (source: NavSource) => void,
  ) {
    this.underline = editor.createDecorationsCollection()
    this.listeners = [
      editor.onMouseMove((e) => {
        this.over = e.target.type === CONTENT_TEXT ? e.target.position : null
        this.held = linkKeyDown(e.event)
        this.refresh()
      }),
      editor.onMouseLeave(() => {
        this.over = null
        this.refresh()
      }),
      editor.onMouseDown((e) => this.press(e)),
      editor.onMouseUp((e) => this.release(e)),
      editor.onDidChangeModel(() => {
        this.over = null
        this.pressed = null
        this.shown = null
        this.underline.clear()
      }),
    ]
    // The key goes up and down wherever the focus is, or in another window.
    window.addEventListener('keydown', this.onKey, true)
    window.addEventListener('keyup', this.onKey, true)
    window.addEventListener('blur', this.onBlur)
  }

  dispose(): void {
    window.removeEventListener('keydown', this.onKey, true)
    window.removeEventListener('keyup', this.onKey, true)
    window.removeEventListener('blur', this.onBlur)
    for (const l of this.listeners) l.dispose()
    this.underline.clear()
    this.shown = null
    this.pressed = null
  }

  private source(e: Monaco.editor.IEditorMouseEvent): NavSource | null {
    if (e.target.type !== CONTENT_TEXT || !e.target.position) return null
    const { clientX, clientY } = e.event.browserEvent
    return sourceAt(this.editor, e.target.position, { x: clientX, y: clientY })
  }

  private press(e: Monaco.editor.IEditorMouseEvent): void {
    this.pressed = null
    if (!e.event.leftButton || e.event.shiftKey || e.event.altKey || !linkKeyDown(e.event)) return
    // Monaco has put the caret there already: a place to come back to.
    this.pressed = this.source(e)
  }

  private release(e: Monaco.editor.IEditorMouseEvent): void {
    const pressed = this.pressed
    this.pressed = null
    if (!pressed || !e.event.leftButton || !linkKeyDown(e.event)) return
    const now = this.source(e)
    if (!now || now.click.line !== pressed.click.line || now.click.column !== pressed.click.column) return
    this.setHeld(false)
    this.onActivate(pressed)
  }

  private setHeld(held: boolean): void {
    if (held === this.held) return
    this.held = held
    this.refresh()
  }

  /** Underlines the name under the pointer while the key is down. */
  private refresh(): void {
    const model = this.editor.getModel()
    const id = this.held && this.over && model ? identifierAt(modelSource(model), this.over) : null
    const key = id ? `${id.line}:${id.column}` : null
    if (key === this.shown) return
    this.shown = key
    this.underline.set(id ? [{ range: { startLineNumber: id.line, startColumn: id.column, endLineNumber: id.line, endColumn: id.endColumn }, options: { inlineClassName: 'tl-nav-link' } }] : [])
  }
}
