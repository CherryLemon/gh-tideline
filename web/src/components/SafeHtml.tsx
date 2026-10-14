import { useMemo } from 'react'
import { sanitizeHtml } from '../sanitize'

/**
 * HTML that GitHub rendered (comment bodies), sanitized first. This is the only place the app
 * sets `innerHTML`; everything else goes through React.
 */
export function SafeHtml({ html, className }: { html: string; className?: string }) {
  const clean = useMemo(() => sanitizeHtml(html), [html])
  return <div className={className ? `tl-md ${className}` : 'tl-md'} dangerouslySetInnerHTML={{ __html: clean }} />
}
