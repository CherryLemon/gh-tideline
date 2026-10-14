// Previews of comments: GitHub renders the text as it would render the comment. The HTML is
// shown through `SafeHtml`, like a comment body.
import { createContext } from 'react'

export interface MarkdownRenderer {
  /** The text's HTML, from GitHub (or the same text's last render). */
  render(text: string): Promise<string>
  /** The text's HTML if it is already rendered, to show it without waiting. */
  peek(text: string): string | undefined
}

/** Where the comment boxes get their previews from. None: the boxes offer no preview. */
export const MarkdownContext = createContext<MarkdownRenderer | null>(null)

/** Texts whose render is kept: going back and forth between Write and Preview asks once. */
const KEPT = 20

/**
 * A renderer that keeps the latest renders, newest last. A text asked for again while GitHub
 * renders it waits for the same answer; a failed render is not kept, so asking again retries.
 */
export function markdownRenderer(fetchHtml: (text: string) => Promise<string>, capacity = KEPT): MarkdownRenderer {
  const pending = new Map<string, Promise<string>>()
  const done = new Map<string, string>()
  const keep = (text: string, html: string) => {
    done.delete(text)
    done.set(text, html)
    for (const oldest of done.keys()) {
      if (done.size <= capacity) break
      done.delete(oldest)
    }
  }
  return {
    render(text) {
      const html = done.get(text)
      if (html !== undefined) {
        keep(text, html)
        return Promise.resolve(html)
      }
      let waiting = pending.get(text)
      if (!waiting) {
        waiting = fetchHtml(text).then(
          (fetched) => {
            pending.delete(text)
            keep(text, fetched)
            return fetched
          },
          (e: unknown) => {
            pending.delete(text)
            throw e
          },
        )
        pending.set(text, waiting)
      }
      return waiting
    },
    peek(text) {
      return done.get(text)
    },
  }
}
