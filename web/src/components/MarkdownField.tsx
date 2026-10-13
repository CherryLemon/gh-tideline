import { useContext, useEffect, useId, useLayoutEffect, useRef, useState } from 'react'
import { ApiError, errorMessage } from '../api'
import { focusWhenShown } from '../focus'
import { MarkdownContext } from '../markdown'
import { SafeHtml } from './SafeHtml'

interface Props {
  /** The text box, for the parent to focus. */
  input: React.RefObject<HTMLTextAreaElement | null>
  value: string
  onChange(value: string): void
  /** Shows the preview in place of the text box. */
  preview: boolean
  onPreview(preview: boolean): void
  /** Says what the text is for, to assistive tech. */
  label: string
  describedBy?: string
  placeholder: string
  readOnly: boolean
  /** Keys in the text box, on the tabs and in the preview. */
  onKeyDown?(e: React.KeyboardEvent): void
}

/** `final`: asking again can't help (the text is too long). */
type Rendered = { text: string; html: string; error?: undefined } | { text: string; html?: undefined; error: string; final: boolean }

/**
 * A comment's text box with Write and Preview tabs: the preview shows the text as GitHub will.
 * Without a renderer (`MarkdownContext`), just the text box.
 */
export function MarkdownField({ input, value, onChange, preview, onPreview, label, describedBy, placeholder, readOnly, onKeyDown }: Props) {
  const md = useContext(MarkdownContext)
  const id = useId()
  const [rendered, setRendered] = useState<Rendered | null>(null)
  /** Bumped by "Try again": a failed render isn't kept, so it is asked for again. */
  const [attempt, setAttempt] = useState(0)
  /** The text box's height when the preview took its place: the box keeps its size. */
  const [height, setHeight] = useState<number>()
  const writeTab = useRef<HTMLButtonElement>(null)
  const previewTab = useRef<HTMLButtonElement>(null)
  const previewBox = useRef<HTMLDivElement>(null)
  /** Set when the text box takes the focus as soon as it shows again (Write clicked). */
  const focusText = useRef(false)
  const blank = value.trim() === ''

  // Before anything else can happen: keys typed right after the click go to the text.
  useLayoutEffect(() => {
    if (preview || !focusText.current) return
    focusText.current = false
    if (input.current) focusWhenShown(input.current)
  }, [preview, input])

  useEffect(() => {
    // A failure isn't kept, so the preview asks again: "Loading preview…" meanwhile, not the old error.
    setRendered((r) => (r?.error !== undefined ? null : r))
    if (!md || !preview || blank) return
    let live = true
    md.render(value).then(
      (html) => live && setRendered({ text: value, html }),
      // 400 (too long) and 413 (too big a request) would only come back.
      (e: unknown) => live && setRendered({ text: value, error: errorMessage(e), final: e instanceof ApiError && (e.status === 400 || e.status === 413) }),
    )
    return () => {
      live = false
    }
  }, [md, preview, blank, value, attempt])

  const textBox = (
    <textarea
      ref={input}
      className="composer-input"
      aria-label={label}
      aria-describedby={describedBy}
      placeholder={placeholder}
      value={value}
      readOnly={readOnly}
      hidden={md !== null && preview}
      onChange={(e) => onChange(e.target.value)}
      onKeyDown={md ? undefined : onKeyDown}
    />
  )
  if (!md) return textBox

  /** Shows the preview (`on`) or the text box; `toTab`: the focus goes to the tab (arrow keys). */
  const show = (on: boolean, toTab: boolean) => {
    if (on && !preview && input.current) setHeight(input.current.offsetHeight)
    if (on !== preview) onPreview(on)
    const tabButton = (on ? previewTab : writeTab).current
    if (toTab || on) {
      if (tabButton) focusWhenShown(tabButton)
    } else if (preview) focusText.current = true
    else if (input.current) focusWhenShown(input.current)
  }
  const onTabKeyDown = (e: React.KeyboardEvent) => {
    if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return
    const on = e.key === 'Home' ? false : e.key === 'End' ? true : e.key === 'ArrowLeft' || e.key === 'ArrowRight' ? !preview : null
    if (on === null) return
    e.preventDefault()
    show(on, true)
  }
  const retry = () => {
    // The button goes away: the focus stays in the box, where the box's keys work.
    previewBox.current?.focus({ preventScroll: true })
    setRendered(null)
    setAttempt((n) => n + 1)
  }

  const cached = preview && !blank ? md.peek(value) : undefined
  const shown = cached !== undefined ? { html: cached, error: undefined } : rendered?.text === value ? rendered : null
  const loading = preview && !blank && shown === null
  return (
    <div className="md-field" onKeyDown={onKeyDown}>
      <div className="md-tabs" role="tablist" aria-label={label} onKeyDown={onTabKeyDown}>
        <button
          ref={writeTab}
          type="button"
          role="tab"
          id={`${id}-write`}
          className="md-tab"
          aria-selected={!preview}
          aria-controls={`${id}-panel`}
          tabIndex={preview ? -1 : 0}
          onClick={() => show(false, false)}
        >
          Write
        </button>
        <button
          ref={previewTab}
          type="button"
          role="tab"
          id={`${id}-preview`}
          className="md-tab"
          aria-selected={preview}
          aria-controls={`${id}-panel`}
          tabIndex={preview ? 0 : -1}
          onClick={() => show(true, false)}
        >
          Preview
        </button>
      </div>
      <div className="md-panel" role="tabpanel" id={`${id}-panel`} aria-labelledby={`${id}-${preview ? 'preview' : 'write'}`}>
        {textBox}
        {preview && (
          // Focusable: a long preview scrolls from the keyboard too.
          <div ref={previewBox} className="md-preview" tabIndex={0} aria-busy={loading} aria-describedby={describedBy} style={{ minHeight: height }}>
            {blank || shown?.html?.trim() === '' ? (
              <p className="muted md-note">Nothing to preview</p>
            ) : shown === null ? (
              <p className="muted md-note">Loading preview…</p>
            ) : shown.error !== undefined ? (
              <div className="composer-error" role="alert">
                Couldn't load the preview: {shown.error}
                {!shown.final && (
                  <>
                    {' '}
                    <button type="button" className="link-btn" onClick={retry}>
                      Try again
                    </button>
                  </>
                )}
              </div>
            ) : (
              <SafeHtml html={shown.html} />
            )}
          </div>
        )}
      </div>
    </div>
  )
}
