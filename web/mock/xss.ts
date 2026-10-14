// Comment HTML that tries every trick a hostile renderer could; it must stay inert in the UI.
// Kept apart from the other fixtures so the sanitizer test can use it without Node APIs.
export const XSS_BODY_HTML =
  '<p dir="auto">Looks fine</p><script>window.__xss = (window.__xss || 0) + 1</script>' +
  '<img src="x" onerror="window.__xss = (window.__xss || 0) + 1">' +
  '<a href="javascript:window.__xss=1">click</a> <a href="https://example.com/ok">ok link</a>' +
  '<style>body { display: none }</style><form action="https://evil.example"><input name="q"></form>' +
  '<iframe src="https://evil.example"></iframe><svg><script>window.__xss=1</script></svg>' +
  '<p style="position:fixed;inset:0;background:red">overlay</p>' +
  '<img src="data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=" alt="data image">'
