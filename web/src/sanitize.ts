// Comment bodies arrive as HTML that GitHub rendered. GitHub already sanitizes it, but it is
// still remote content shown inside the app's page, so it goes through a dedicated DOMPurify
// instance before it reaches the DOM.
import DOMPurify, { type DOMPurify as Purifier } from 'dompurify'

/** Tags removed with their content on top of DOMPurify's defaults. */
const FORBID_TAGS = ['style', 'form', 'iframe', 'button', 'select', 'textarea', 'option', 'dialog', 'template']
/**
 * `style` could restyle or overlay the app; `id` and `name` could clobber the app's globals;
 * `srcset` would bypass the https check on `src`.
 */
const FORBID_ATTR = ['style', 'id', 'name', 'srcset', 'download', 'action', 'background']
/** Attributes that load or link to a URL; only absolute https URLs are kept. */
const URL_ATTRS = ['href', 'xlink:href', 'src', 'poster', 'cite']
/** Elements that are pointless without their `src`, so they go when it does. */
const MEDIA = new Set(['img', 'video', 'audio', 'source', 'track'])
/**
 * Class names GitHub's renderer emits and the comment styles may use. Any other class could
 * pick up one of the app's own styles.
 */
const CLASS_ALLOWED = /^(?:pl-[a-z0-9]+|highlight(?:-[a-z0-9-]+)?|emoji|task-list-item(?:-checkbox)?|contains-task-list|user-mention|team-mention|markdown-alert(?:-[a-z]+)?)$/

function isHttps(value: string): boolean {
  try {
    return new URL(value).protocol === 'https:'
  } catch {
    return false
  }
}

function clean(node: Element): void {
  for (const attr of URL_ATTRS) {
    const value = node.getAttribute(attr)
    if (value !== null && !isHttps(value.trim())) node.removeAttribute(attr)
  }
  if (MEDIA.has(node.localName) && !node.hasAttribute('src')) {
    node.remove()
    return
  }
  const classes = node.getAttribute('class')
  if (classes !== null) {
    const kept = classes.split(/\s+/).filter((c) => CLASS_ALLOWED.test(c))
    if (kept.length) node.setAttribute('class', kept.join(' '))
    else node.removeAttribute('class')
  }
  if (node.localName === 'a' && node.hasAttribute('href')) {
    // Removed first so the attributes come out in the same order on every pass.
    node.removeAttribute('rel')
    node.setAttribute('target', '_blank')
    node.setAttribute('rel', 'noopener noreferrer')
  }
  if (node.localName === 'input') {
    // Task-list boxes stay as a read-only display; any other input goes.
    if (node.getAttribute('type') !== 'checkbox') node.remove()
    else node.setAttribute('disabled', '')
  }
}

let purifier: Purifier | null = null

function instance(): Purifier {
  if (!purifier) {
    purifier = DOMPurify(window)
    purifier.addHook('afterSanitizeAttributes', clean)
  }
  return purifier
}

/** GitHub-rendered HTML made safe to insert into the page. */
export function sanitizeHtml(html: string): string {
  return instance().sanitize(html, {
    FORBID_TAGS,
    FORBID_ATTR,
    ALLOW_DATA_ATTR: false,
  })
}
