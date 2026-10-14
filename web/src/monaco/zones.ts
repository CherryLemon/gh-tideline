// Cards (review threads, comment boxes) between the lines of the diff editor, the way VS Code's
// ZoneWidget does it: an empty view zone makes room under a line, and the card itself is an
// overlay widget moved to the zone's top. So the card doesn't scroll sideways with the code,
// isn't stretched by long lines, and clicks and text selection work normally (S8).
import type * as Monaco from 'monaco-editor/editor/editor.api'
import type { EditorSide } from '../threads'

type DiffEditorT = Monaco.editor.IStandaloneDiffEditor
type CodeEditorT = Monaco.editor.ICodeEditor

export interface CardSpec {
  /** Stable across rebuilds: the same key keeps its DOM node, so React state and focus survive. */
  key: string
  editor: EditorSide
  afterLine: number
  ordinal: number
  /** Line of the modified editor to unfold first, if any. */
  reveal: number | null
}

interface Card {
  key: string
  /** The overlay widget's node: positioned by us, and the event boundary. */
  outer: HTMLDivElement
  /** The portal container React renders into. */
  inner: HTMLDivElement
  spec: CardSpec | null
  /** The editor holding the overlay widget. */
  host: CodeEditorT | null
  widget: Monaco.editor.IOverlayWidget | null
  zone: Monaco.editor.IViewZone | null
  zoneId: string | null
  /** Last height measured at a real width. */
  height: number | null
  /** Last top given to the card. */
  top: number
}

/** Space a card gets before it has been measured. */
const ESTIMATED_HEIGHT = 60
/** Where a card waits off screen for its zone to report a top. */
const PARKED_TOP = -100000
/** Room kept between a card brought into view and the edge of the code area. */
const REVEAL_MARGIN = 12

/**
 * Events that must not reach Monaco (its keybindings listen on the editor's container, its
 * mouse handling and context menu on the view) or the page's bubbling handlers. React still
 * gets them: it listens on the portal container inside. The page's capture-phase shortcuts
 * see them anyway; `isTypingTarget` counts text fields in cards as typing.
 */
const ISOLATED_EVENTS = [
  'keydown',
  'keyup',
  'keypress',
  'beforeinput',
  'input',
  'compositionstart',
  'compositionupdate',
  'compositionend',
  'copy',
  'cut',
  'paste',
  'pointerdown',
  'mousedown',
  'dblclick',
  'contextmenu',
] as const

const stop = (e: Event) => e.stopPropagation()

/** Whether `el` or an ancestor below `boundary` can scroll in the wheel's direction. */
function scrollsItself(el: Element | null, boundary: Element, e: WheelEvent): boolean {
  for (; el && el !== boundary; el = el.parentElement) {
    const style = getComputedStyle(el)
    const canY = /auto|scroll/.test(style.overflowY) && el.scrollHeight > el.clientHeight
    const canX = /auto|scroll/.test(style.overflowX) && el.scrollWidth > el.clientWidth
    if (canY && e.deltaY !== 0) {
      const atEnd = e.deltaY > 0 ? el.scrollTop + el.clientHeight >= el.scrollHeight - 1 : el.scrollTop <= 0
      if (!atEnd) return true
    }
    if (canX && e.deltaX !== 0) {
      const atEnd = e.deltaX > 0 ? el.scrollLeft + el.clientWidth >= el.scrollWidth - 1 : el.scrollLeft <= 0
      if (!atEnd) return true
    }
  }
  return false
}

/** Width of the code area: from the end of the gutter to the scrollbar. */
function codeWidth(ed: CodeEditorT): number {
  const info = ed.getLayoutInfo()
  return info.width - info.contentLeft - info.verticalScrollbarWidth - info.minimap.minimapWidth
}

export class ZoneHost {
  private readonly cards = new Map<string, Card>()
  private readonly byOuter = new Map<Element, Card>()
  private readonly observer: ResizeObserver
  private readonly disposables: Monaco.IDisposable[] = []

  /** `topInset`: how far down the first lines of an editor are covered by something over its code (a sticky header). */
  constructor(
    private readonly diff: DiffEditorT,
    private readonly topInset: (ed: CodeEditorT) => number = () => 0,
  ) {
    this.observer = new ResizeObserver((entries) => this.onResize(entries))
    for (const ed of [diff.getOriginalEditor(), diff.getModifiedEditor()]) {
      this.disposables.push(ed.onDidLayoutChange(() => this.layoutWidths(ed)))
      // A new model drops that editor's view zones; its cards wait, hidden, for the next sync.
      // An editor given the model it already has (one side unchanged) keeps its zones.
      this.disposables.push(ed.onDidChangeModel(() => this.reset(ed)))
    }
  }

  /** The node to render a card into; it exists from the first call until the card is synced away. */
  node(key: string): HTMLElement {
    return this.card(key).inner
  }

  /**
   * Shows exactly the cards in `specs`. All zones are rebuilt: lines are unfolded first and
   * zones added after, or the other side of a split diff doesn't get matching space (S8).
   * `cursor`: where to leave the modified editor's cursor after unfolding: `keep` puts the
   * user's selection back; otherwise it goes to `cursor` (a line), since restoring the
   * untouched initial (1, 1) would unfold the top of the file.
   */
  sync(specs: readonly CardSpec[], cursor: 'keep' | number): void {
    const focus = this.saveFocus()
    // Removing the zones shortens the content, which clamps the scroll position, and cards
    // added above the viewport would push the code down: keep the top line where it is.
    const viewport = this.saveViewport()
    const wanted = new Set(specs.map((s) => s.key))
    for (const ed of this.editors()) {
      ed.changeViewZones((acc) => {
        for (const c of this.cards.values()) {
          if (c.host === ed && c.zoneId !== null) acc.removeZone(c.zoneId)
        }
      })
    }
    for (const c of this.cards.values()) {
      c.zone = null
      c.zoneId = null
    }
    for (const c of [...this.cards.values()]) if (!wanted.has(c.key)) this.drop(c)

    this.reveal(specs, cursor)

    for (const spec of specs) {
      const c = this.card(spec.key)
      c.spec = spec
      const ed = this.editorFor(spec.editor)
      if (c.host !== ed) this.attach(c, ed)
    }
    for (const ed of this.editors()) {
      ed.changeViewZones((acc) => {
        for (const c of this.cards.values()) {
          if (c.host !== ed || !c.spec) continue
          const zone: Monaco.editor.IViewZone = {
            afterLineNumber: c.spec.afterLine,
            ordinal: c.spec.ordinal,
            heightInPx: this.measure(c) ?? c.height ?? ESTIMATED_HEIGHT,
            // The space the card's overlay is drawn over.
            domNode: Object.assign(document.createElement('div'), { className: 'tl-zone-space' }),
            onDomNodeTop: (top) => this.place(c, top),
          }
          c.zone = zone
          c.zoneId = acc.addZone(zone)
        }
      })
    }
    this.restoreViewport(viewport)
    this.restoreFocus(focus)
  }

  /**
   * Scrolls the code so the card `key` shows, or its top when it is taller than the code area.
   * Works before Monaco has drawn the card: from its zone's place and height.
   */
  revealCard(key: string): void {
    const c = this.cards.get(key)
    const ed = c?.host
    if (!c?.spec || !c.zone || !ed) return
    const { afterLine, ordinal } = c.spec
    // The zone starts under its line (0: at the top), after the cards there that come first.
    let top = afterLine > 0 ? ed.getBottomForLineNumber(afterLine) : 0
    for (const o of this.cards.values()) {
      if (o !== c && o.host === ed && o.zone && o.spec?.afterLine === afterLine && o.spec.ordinal < ordinal) top += o.zone.heightInPx ?? 0
    }
    this.scrollToShow(ed, top, top + (c.zone.heightInPx ?? 0))
  }

  dispose(): void {
    for (const c of [...this.cards.values()]) this.drop(c)
    this.observer.disconnect()
    for (const d of this.disposables) d.dispose()
  }

  /** The zones are gone (the model changed): hide the cards, keeping their nodes. */
  private reset(ed: CodeEditorT): void {
    for (const c of this.cards.values()) {
      if (c.host !== ed) continue
      c.zone = null
      c.zoneId = null
      this.place(c, PARKED_TOP)
    }
  }

  /**
   * Moves a card to its zone's top. Monaco reports a far negative top for zones out of view;
   * those cards are hidden so Tab can't land in them, unless focus is already inside.
   */
  private place(c: Card, top: number): void {
    c.top = top
    c.outer.style.top = `${top}px`
    const away = top <= PARKED_TOP && !c.outer.contains(document.activeElement)
    c.outer.style.visibility = away ? 'hidden' : ''
  }

  private editors(): CodeEditorT[] {
    return [this.diff.getOriginalEditor(), this.diff.getModifiedEditor()]
  }

  private editorFor(side: EditorSide): CodeEditorT {
    return side === 'original' ? this.diff.getOriginalEditor() : this.diff.getModifiedEditor()
  }

  private card(key: string): Card {
    let c = this.cards.get(key)
    if (c) return c
    const outer = document.createElement('div')
    outer.className = 'tl-zone'
    const inner = document.createElement('div')
    inner.className = 'tl-zone-inner'
    outer.appendChild(inner)
    for (const type of ISOLATED_EVENTS) outer.addEventListener(type, stop)
    c = { key, outer, inner, spec: null, host: null, widget: null, zone: null, zoneId: null, height: null, top: PARKED_TOP }
    const card = c
    outer.addEventListener('wheel', (e) => this.onWheel(card, e), { passive: false })
    // A card kept visible off screen for its focus hides once focus leaves it.
    outer.addEventListener('focusout', (e) => {
      if (!outer.contains(e.relatedTarget as Node | null)) setTimeout(() => this.cards.get(key) === card && this.place(card, card.top))
    })
    // What the page would do for a field it scrolls: keep the focused field (and a growing
    // text box) in view. Not for a button pressed with the mouse: scrolling under the pointer
    // would move the button away before its click.
    outer.addEventListener('focusin', (e) => {
      if (e.target instanceof Element && e.target.matches(':focus-visible')) this.keepInView(card, e.target, 'top')
    })
    outer.addEventListener('input', (e) => this.keepInView(card, e.target, 'bottom'))
    this.cards.set(key, c)
    this.byOuter.set(outer, c)
    return c
  }

  private attach(c: Card, ed: CodeEditorT): void {
    if (c.host && c.widget) c.host.removeOverlayWidget(c.widget)
    else this.observer.observe(c.outer)
    const widget: Monaco.editor.IOverlayWidget = {
      getId: () => `tl-zone:${c.key}`,
      getDomNode: () => c.outer,
      getPosition: () => null,
    }
    c.host = ed
    c.widget = widget
    this.place(c, PARKED_TOP)
    ed.addOverlayWidget(widget)
    this.layoutWidth(c)
  }

  private drop(c: Card): void {
    if (c.host && c.widget) c.host.removeOverlayWidget(c.widget)
    this.observer.unobserve(c.outer)
    this.cards.delete(c.key)
    this.byOuter.delete(c.outer)
  }

  /** Unfolds the lines the cards hang on, from the modified editor (S8: the only one that can). */
  private reveal(specs: readonly CardSpec[], cursor: 'keep' | number): void {
    const ed = this.diff.getModifiedEditor()
    const model = ed.getModel()
    const lines = [...new Set(specs.map((s) => s.reveal).filter((n): n is number => n !== null && n > 0))]
    if (!model || lines.length === 0) return
    const last = model.getLineCount()
    const before = ed.getSelections()
    // A zone under line N hides when N + 1 is folded too, so select both.
    ed.setSelections(
      lines.map((n) => {
        const l = Math.min(n, last)
        const next = Math.min(l + 1, last)
        return { selectionStartLineNumber: l, selectionStartColumn: 1, positionLineNumber: next, positionColumn: 1 }
      }),
    )
    if (cursor === 'keep' && before) ed.setSelections(before)
    else if (cursor !== 'keep') ed.setPosition({ lineNumber: Math.min(Math.max(1, cursor), last), column: 1 })
  }

  private onResize(entries: ResizeObserverEntry[]): void {
    const dirty = new Set<Card>()
    for (const e of entries) {
      const c = this.byOuter.get(e.target)
      if (c?.zone && c.zoneId !== null) dirty.add(c)
    }
    for (const ed of this.editors()) {
      const mine = [...dirty].filter((c) => c.host === ed)
      if (mine.length === 0) continue
      ed.changeViewZones((acc) => {
        for (const c of mine) {
          const h = this.measure(c)
          if (!c.zone || c.zoneId === null || h === null || h === c.zone.heightInPx) continue
          c.zone.heightInPx = h
          acc.layoutZone(c.zoneId)
        }
      })
    }
  }

  /** The card's height, if it is laid out at a real width (the hidden editor has none). */
  private measure(c: Card): number | null {
    if (!c.host || codeWidth(c.host) <= 0) return null
    const h = c.outer.offsetHeight
    if (h === 0) return null
    c.height = h
    return h
  }

  /** The modified editor's top line and its offset (it scrolls the original one along). */
  private saveViewport(): { line: number; offset: number } | null {
    const ed = this.diff.getModifiedEditor()
    const top = ed.getScrollTop()
    const line = ed.getVisibleRanges()[0]?.startLineNumber
    // At the very top, stay there: cards above line 1 should show.
    if (top === 0 || line === undefined) return null
    return { line, offset: ed.getTopForLineNumber(line) - top }
  }

  private restoreViewport(saved: ReturnType<ZoneHost['saveViewport']>): void {
    if (!saved) return
    const ed = this.diff.getModifiedEditor()
    const top = ed.getTopForLineNumber(saved.line) - saved.offset
    if (top !== ed.getScrollTop()) ed.setScrollTop(top)
  }

  private layoutWidths(ed: CodeEditorT): void {
    for (const c of this.cards.values()) if (c.host === ed) this.layoutWidth(c)
  }

  /** Over the code area only: from the end of the gutter to the scrollbar. */
  private layoutWidth(c: Card): void {
    if (!c.host) return
    c.outer.style.left = `${c.host.getLayoutInfo().contentLeft}px`
    c.outer.style.width = `${Math.max(0, codeWidth(c.host))}px`
  }

  /**
   * Scrolls `ed` as little as needed to show `top`..`bottom` (content offsets); when they don't
   * fit, `wins` shows.
   */
  private scrollToShow(ed: CodeEditorT, top: number, bottom: number, wins: 'top' | 'bottom' = 'top'): void {
    const info = ed.getLayoutInfo()
    const height = info.height - info.horizontalScrollbarHeight
    const showBottom = (from: number) => (bottom > from + height - REVEAL_MARGIN ? bottom - height + REVEAL_MARGIN : from)
    // What covers the top of the code depends on where the code is scrolled to: look again from where it ended up.
    for (let round = 0; round < 3; round++) {
      const scroll = ed.getScrollTop()
      const inset = this.topInset(ed) + REVEAL_MARGIN
      const showTop = (from: number) => (top < from + inset ? top - inset : from)
      const next = Math.max(0, wins === 'top' ? showTop(showBottom(scroll)) : showBottom(showTop(scroll)))
      if (next === scroll) return
      ed.setScrollTop(next)
    }
  }

  /**
   * Brings `target`, in card `c`, into view when it is shown (not parked) and partly hidden;
   * `wins` when it doesn't fit (the end of a text box being typed in).
   */
  private keepInView(c: Card, target: EventTarget | null, wins: 'top' | 'bottom'): void {
    const ed = c.host
    const box = ed?.getDomNode()
    if (!ed || !box || !(target instanceof HTMLElement) || c.top <= PARKED_TOP) return
    const view = box.getBoundingClientRect()
    const r = target.getBoundingClientRect()
    const scroll = ed.getScrollTop()
    this.scrollToShow(ed, scroll + r.top - view.top, scroll + r.bottom - view.top, wins)
  }

  /** The wheel scrolls the code under the card, unless something in the card can scroll. */
  private onWheel(c: Card, e: WheelEvent): void {
    if (!c.host || e.ctrlKey || scrollsItself(e.target as Element, c.outer, e)) return
    const delegate = (c.host as CodeEditorT & { delegateScrollFromMouseWheelEvent?(e: WheelEvent): void })
      .delegateScrollFromMouseWheelEvent
    if (typeof delegate !== 'function') return
    // Monaco skips wheel events whose default is prevented, so prevent it only afterwards.
    delegate.call(c.host, e)
    e.preventDefault()
  }

  private saveFocus(): { el: HTMLElement; start: number | null; end: number | null } | null {
    const el = document.activeElement
    if (!(el instanceof HTMLElement)) return null
    if (![...this.cards.values()].some((c) => c.outer.contains(el))) return null
    const field = el instanceof HTMLTextAreaElement || el instanceof HTMLInputElement ? el : null
    return { el, start: field?.selectionStart ?? null, end: field?.selectionEnd ?? null }
  }

  /** Moving a card to the other editor re-parents its node, which drops focus. */
  private restoreFocus(saved: ReturnType<ZoneHost['saveFocus']>): void {
    if (!saved || document.activeElement === saved.el || !saved.el.isConnected) return
    saved.el.focus({ preventScroll: true })
    if (saved.start !== null && (saved.el instanceof HTMLTextAreaElement || saved.el instanceof HTMLInputElement)) {
      saved.el.setSelectionRange(saved.start, saved.end ?? saved.start)
    }
  }
}
