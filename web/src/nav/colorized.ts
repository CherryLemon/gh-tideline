// A source line as Monaco colorizes it, read back as plain data: the sticky header draws repository text, which must
// never be handed to innerHTML (design.md §8).

/** A stretch of a line in one color; `className` is empty or Monaco's token classes (`mtk3`, `mtki`…). */
export interface TextRun {
  text: string
  className: string
}

const TOKEN_CLASSES = /^(?:mtk\d+|mtk[ibus])(?: (?:mtk\d+|mtk[ibus]))*$/

/**
 * The runs of text in the HTML `monaco.editor.colorize` returns for a line. The markup is parsed into an inert document
 * (nothing in it runs or loads) and only its text and token classes are kept, so what comes out cannot carry an element
 * or an attribute of the repository's making. Monaco writes spaces as no-break spaces; they are spaces again here.
 */
export function colorizedRuns(html: string): TextRun[] {
  const doc = new DOMParser().parseFromString(html, 'text/html')
  const runs: TextRun[] = []
  for (const el of doc.body.querySelectorAll('span')) {
    if (el.childElementCount > 0) continue
    const text = (el.textContent ?? '').replaceAll('\u00a0', ' ')
    if (text === '') continue
    const className = el.getAttribute('class') ?? ''
    runs.push({ text, className: TOKEN_CLASSES.test(className) ? className : '' })
  }
  return runs
}
