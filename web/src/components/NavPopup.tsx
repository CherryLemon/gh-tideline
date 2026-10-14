import { useEffect, useId, useLayoutEffect, useRef, useState } from 'react'
import type { NavHit } from '../api'
import { focusWhenShown } from '../focus'

export interface NavPopupProps {
  /** What the list is: "Choose declaration of x (2)". */
  title: string
  hits: readonly NavHit[]
  /** Said when there are no hits. */
  empty?: string
  /** A line under the list. */
  note?: string
  /** Where the list opens, in window coordinates: at the click, or at the caret. */
  anchor: { x: number; y: number }
  onPick(hit: NavHit): void
  onClose(): void
}

/** Space kept to the window's edges. */
const MARGIN = 8
/** Between the anchor and the list below it. */
const GAP = 4
/** From the anchor to the bottom of a list above it: clears the line the anchor is on. */
const FLIP = 24
/** Rows PageUp and PageDown move by. */
const PAGE = 8

/** The line of a hit, with the name in it marked (the index counts the columns in UTF-16 units, as the page's strings do). */
function Snippet({ hit }: { hit: NavHit }) {
  const at = hit.snippetColumn - 1
  const end = at + hit.endColumn - hit.column
  if (at < 0 || end <= at || end > hit.snippet.length) return <>{hit.snippet}</>
  return (
    <>
      {hit.snippet.slice(0, at)}
      <mark>{hit.snippet.slice(at, end)}</mark>
      {hit.snippet.slice(end)}
    </>
  )
}

/**
 * A list of places to choose one from, anchored at the click that asked for it, like a menu: the first row is active
 * and has the focus, ↑ ↓ Home End PageUp PageDown move, Enter or a click opens, Esc or a press outside closes (focus goes
 * back to where it was). What the rows show comes from the repository, so it is put on the page as text.
 */
export function NavPopup({ title, hits, empty, note, anchor, onPick, onClose }: NavPopupProps) {
  const id = useId()
  const root = useRef<HTMLDivElement>(null)
  const list = useRef<HTMLDivElement>(null)
  const [active, setActive] = useState(0)
  const [place, setPlace] = useState({ left: anchor.x, top: anchor.y + GAP })
  // Read while rendering, before the list takes focus: focus goes back there on closing (the editor).
  const [opener] = useState(() => document.activeElement)

  useEffect(
    () => () => {
      const lost = document.activeElement === null || document.activeElement === document.body
      if (lost && opener instanceof HTMLElement && opener !== document.body && opener.isConnected) opener.focus({ preventScroll: true })
    },
    [opener],
  )
  useEffect(() => {
    const target = list.current ?? root.current
    if (target) focusWhenShown(target)
  }, [])
  useEffect(() => {
    const onDown = (e: PointerEvent) => {
      if (!(e.target instanceof Node && root.current?.contains(e.target))) onClose()
    }
    document.addEventListener('pointerdown', onDown, true)
    window.addEventListener('resize', onClose)
    return () => {
      document.removeEventListener('pointerdown', onDown, true)
      window.removeEventListener('resize', onClose)
    }
  }, [onClose])
  // Below the anchor, or above it where there is no room; always inside the window.
  useLayoutEffect(() => {
    const box = root.current?.getBoundingClientRect()
    if (!box) return
    const below = anchor.y + GAP
    const above = anchor.y - FLIP - box.height
    const vh = document.documentElement.clientHeight
    const left = Math.max(MARGIN, Math.min(anchor.x, document.documentElement.clientWidth - box.width - MARGIN))
    const top = below + box.height + MARGIN <= vh ? below : above >= MARGIN ? above : Math.max(MARGIN, vh - box.height - MARGIN)
    setPlace((p) => (p.left === left && p.top === top ? p : { left, top }))
  }, [anchor, hits.length])
  useEffect(() => {
    list.current?.querySelector('.is-active')?.scrollIntoView({ block: 'nearest' })
  }, [active])

  const onKeyDown = (e: React.KeyboardEvent) => {
    // Also Safari's Esc ending a composition (`isComposing` already false there).
    if (e.nativeEvent.isComposing || e.keyCode === 229 || e.ctrlKey || e.altKey || e.metaKey) return
    const move = (to: number) => {
      e.preventDefault()
      setActive(Math.max(0, Math.min(to, hits.length - 1)))
    }
    switch (e.key) {
      case 'ArrowDown':
        return move(active + 1)
      case 'ArrowUp':
        return move(active - 1)
      case 'PageDown':
        return move(active + PAGE)
      case 'PageUp':
        return move(active - PAGE)
      case 'Home':
        return move(0)
      case 'End':
        return move(hits.length - 1)
      case 'Enter':
        e.preventDefault()
        if (hits[active]) onPick(hits[active])
        return
      case 'Escape':
        e.preventDefault()
        return onClose()
      case 'Tab':
        // The list is the only stop: the page behind keeps its keys to itself while it is open.
        e.preventDefault()
    }
  }

  return (
    <div
      ref={root}
      className="nav-popup"
      role="dialog"
      aria-label={title}
      tabIndex={-1}
      style={{ left: place.left, top: place.top }}
      onKeyDown={onKeyDown}
    >
      <div className="nav-popup-title">
        <span>{title}</span>
        {hits.length > 0 && <span className="muted">↑↓ select · Enter open · Esc close</span>}
      </div>
      {hits.length > 0 ? (
        <div
          ref={list}
          className="nav-popup-list"
          role="listbox"
          aria-label={title}
          aria-activedescendant={`${id}-${active}`}
          tabIndex={-1}
        >
          {hits.map((hit, i) => {
            const slash = hit.path.lastIndexOf('/') + 1
            return (
              <div
                key={i}
                id={`${id}-${i}`}
                className={`nav-popup-item${i === active ? ' is-active' : ''}`}
                role="option"
                aria-selected={i === active}
                title={`${hit.path}:${hit.line}`}
                onMouseMove={() => setActive(i)}
                onClick={() => onPick(hit)}
              >
                <span className="nav-popup-file">{hit.path.slice(slash)}</span>
                <span className="nav-popup-dir muted">{hit.path.slice(0, slash)}</span>
                <span className="nav-popup-line muted">:{hit.line}</span>
                <code className="nav-popup-snippet">
                  <Snippet hit={hit} />
                </code>
              </div>
            )
          })}
        </div>
      ) : (
        <div className="nav-popup-empty muted">{empty ?? 'Nothing found'}</div>
      )}
      {note && <div className="nav-popup-note muted">{note}</div>}
    </div>
  )
}
