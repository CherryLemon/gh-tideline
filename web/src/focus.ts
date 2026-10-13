/** Frames to wait for a new box to show (a box in the diff waits for its zone) before focusing. */
const FOCUS_FRAMES = 30

/**
 * Focuses `el` once it can take focus. Inside the diff editor the page must not scroll: the
 * editor scrolls itself (`ZoneHost`), and a scrolled editor container misplaces everything.
 */
export function focusWhenShown(el: HTMLElement, frames = FOCUS_FRAMES): void {
  if (!el.isConnected) return
  el.focus({ preventScroll: !!el.closest('.tl-zone') })
  if (document.activeElement !== el && frames > 0) requestAnimationFrame(() => focusWhenShown(el, frames - 1))
}
