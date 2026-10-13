// @vitest-environment jsdom
import { describe, expect, it } from 'vitest'
import { XSS_BODY_HTML } from '../mock/xss.ts'
import { sanitizeHtml } from './sanitize'

const DANGEROUS_TAGS = new Set(['script', 'style', 'iframe', 'form', 'object', 'embed', 'textarea', 'button', 'select', 'base', 'link', 'meta'])
const URL_ATTRS = ['href', 'xlink:href', 'src', 'poster', 'cite', 'action', 'formaction', 'srcset']

/** Parses the output again and checks nothing in it can run, load or restyle anything. */
function expectInert(html: string): void {
  const out = sanitizeHtml(html)
  const box = document.createElement('div')
  box.innerHTML = out
  for (const el of box.querySelectorAll('*')) {
    expect(DANGEROUS_TAGS.has(el.localName), `<${el.localName}> in ${out}`).toBe(false)
    for (const attr of el.attributes) {
      expect(attr.name.startsWith('on'), `${attr.name} in ${out}`).toBe(false)
      expect(['style', 'id', 'name'].includes(attr.name), `${attr.name} in ${out}`).toBe(false)
      if (URL_ATTRS.includes(attr.name)) expect(attr.value, `${attr.name} in ${out}`).toMatch(/^https:\/\//)
    }
    if (el.localName === 'input') {
      expect(el.getAttribute('type')).toBe('checkbox')
      expect(el.hasAttribute('disabled')).toBe(true)
    }
  }
  // Sanitizing is stable: a second pass finds nothing more to remove.
  expect(sanitizeHtml(out)).toBe(out)
}

const VECTORS = [
  '<script>window.__xss = 1</script>',
  '<img src="x" onerror="window.__xss = 1">',
  '<a href="javascript:window.__xss=1">click</a>',
  '<a href=" jAvAsCrIpT:alert(1)">click</a>',
  '<a href="data:text/html,<script>alert(1)</script>">click</a>',
  '<a href="vbscript:alert(1)">click</a>',
  '<style>body { display: none }</style>',
  '<form action="https://evil.example"><input name="q"><button formaction="https://evil.example">go</button></form>',
  '<iframe src="https://evil.example"></iframe>',
  '<svg><script>window.__xss = 1</script></svg>',
  '<svg><a xlink:href="javascript:alert(1)"><text>x</text></a></svg>',
  '<svg><animate onbegin="alert(1)" attributeName="x"></animate></svg>',
  '<math><mtext><table><mglyph><style><!--</style><img title="--&gt;&lt;img src=1 onerror=alert(1)&gt;">',
  '<noscript><p title="</noscript><img src=x onerror=alert(1)>">',
  '<svg></p><style><a id="</style><img src=1 onerror=alert(1)>">',
  '<p style="position:fixed;inset:0;background:red">overlay</p>',
  '<img src="data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=">',
  '<img src="http://example.com/a.png">',
  '<img src="https://example.com/a.png" srcset="http://evil.example/a.png 2x">',
  '<video poster="javascript:alert(1)" src="https://example.com/v.mp4"></video>',
  '<object data="https://evil.example/x.swf"></object><embed src="https://evil.example/x.swf">',
  '<base href="https://evil.example/"><link rel="stylesheet" href="https://evil.example/x.css">',
  '<meta http-equiv="refresh" content="0;url=https://evil.example">',
  '<img name="getElementById"><form id="location"></form><a id="defaultView" href="https://x.example">x</a>',
  '<input type="text" value="phish"><input type="checkbox" checked>',
  '<details open ontoggle="alert(1)"><summary>s</summary>x</details>',
  '<blockquote cite="javascript:alert(1)">q</blockquote>',
  '<a href="/relative">rel</a><a href="//evil.example">proto-relative</a><a href="#frag">frag</a>',
]

describe('sanitizeHtml', () => {
  it.each(VECTORS)('leaves nothing active in %s', (html) => expectInert(html))

  it('drops script and style, keeping the text around them', () => {
    expect(sanitizeHtml('<p>Looks fine</p><script>alert(1)</script><style>p{}</style>')).toBe('<p>Looks fine</p>')
  })

  it('keeps https links and opens them in a new tab without a referrer', () => {
    expect(sanitizeHtml('<a href="https://example.com/ok" rel="nofollow" target="_self">ok</a>')).toBe(
      '<a href="https://example.com/ok" target="_blank" rel="noopener noreferrer">ok</a>',
    )
  })

  it('turns links with other schemes or relative targets into plain text', () => {
    expect(sanitizeHtml('<a href="javascript:alert(1)">a</a>')).toBe('<a>a</a>')
    expect(sanitizeHtml('<a href="mailto:a@example.com">m</a>')).toBe('<a>m</a>')
    expect(sanitizeHtml('<a href="/x">r</a>')).toBe('<a>r</a>')
  })

  it('removes images and media that are not on https', () => {
    expect(sanitizeHtml('<p>a<img src="data:image/png;base64,AAAA" alt="d">b</p>')).toBe('<p>ab</p>')
    expect(sanitizeHtml('<img src="http://example.com/a.png">')).toBe('')
    expect(sanitizeHtml('<video src="blob:https://x/1"></video>')).toBe('')
  })

  it('keeps task-list checkboxes but disables them', () => {
    const out = sanitizeHtml('<ul class="contains-task-list"><li class="task-list-item"><input type="checkbox" class="task-list-item-checkbox" checked> done</li></ul>')
    const box = document.createElement('div')
    box.innerHTML = out
    const input = box.querySelector('input')!
    expect(input.disabled).toBe(true)
    expect(input.checked).toBe(true)
    expect(box.querySelector('ul')!.className).toBe('contains-task-list')
  })

  it('keeps only class names from GitHub’s renderer', () => {
    expect(sanitizeHtml('<div class="toast modal highlight highlight-source-ts"><span class="pl-k topbar">const</span></div>')).toBe(
      '<div class="highlight highlight-source-ts"><span class="pl-k">const</span></div>',
    )
    expect(sanitizeHtml('<p class="statusbar">x</p>')).toBe('<p>x</p>')
  })

  it('drops data attributes, ids and names', () => {
    expect(sanitizeHtml('<p data-path="src/a.ts" id="root" title="t">x</p>')).toBe('<p title="t">x</p>')
  })

  it('keeps typical GitHub output intact', () => {
    const html =
      '<p dir="auto">See <a class="user-mention notranslate" href="https://github.com/alice">@alice</a> ' +
      '<img class="emoji" title=":+1:" alt=":+1:" src="https://github.githubassets.com/images/icons/emoji/unicode/1f44d.png" height="20" width="20" align="absmiddle"></p>\n' +
      '<div class="highlight highlight-source-rust"><pre><code><span class="pl-k">let</span> x = <span class="pl-c1">1</span>;</code></pre></div>\n' +
      '<blockquote><p dir="auto">quoted</p></blockquote>\n' +
      '<table><thead><tr><th>a</th></tr></thead><tbody><tr><td><code>b</code></td></tr></tbody></table>\n' +
      '<details><summary>More</summary><p dir="auto">hidden <del>old</del> <strong>new</strong></p></details>'
    const out = sanitizeHtml(html)
    expect(out).toContain('<a class="user-mention" href="https://github.com/alice" target="_blank" rel="noopener noreferrer">@alice</a>')
    expect(out).toContain('<img class="emoji" title=":+1:" alt=":+1:" src="https://github.githubassets.com/images/icons/emoji/unicode/1f44d.png" height="20" width="20" align="absmiddle">')
    expect(out).toContain('<span class="pl-k">let</span> x = <span class="pl-c1">1</span>;')
    expect(out).toContain('<blockquote><p dir="auto">quoted</p></blockquote>')
    expect(out).toContain('<td><code>b</code></td>')
    expect(out).toContain('<details><summary>More</summary><p dir="auto">hidden <del>old</del> <strong>new</strong></p></details>')
  })

  it('keeps the mock XSS fixture inert', () => {
    expectInert(XSS_BODY_HTML)
    const out = sanitizeHtml(XSS_BODY_HTML)
    expect(out).toContain('<a href="https://example.com/ok" target="_blank" rel="noopener noreferrer">ok link</a>')
    expect(out).not.toContain('evil.example')
  })
})
