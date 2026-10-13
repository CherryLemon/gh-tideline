// Builders for unit tests.
import type { ReviewThread } from './api'

let seq = 0
/** A one-comment RIGHT thread on line 10 of src/a.ts; ids and times differ per call. */
export function mkThread(over: Partial<ReviewThread> = {}): ReviewThread {
  seq++
  return {
    id: `T${seq}`,
    path: 'src/a.ts',
    subjectType: 'LINE',
    side: 'RIGHT',
    startSide: null,
    line: 10,
    startLine: null,
    originalLine: 10,
    originalStartLine: null,
    diffHunk: '',
    isResolved: false,
    isOutdated: false,
    isCollapsed: false,
    resolvedBy: null,
    viewerCanReply: true,
    viewerCanResolve: true,
    viewerCanUnresolve: false,
    comments: [
      {
        id: `C${seq}`,
        author: { login: 'alice', avatarUrl: null },
        body: 'b',
        bodyHtml: '<p>b</p>',
        createdAt: `2026-10-13T00:00:${String(seq % 60).padStart(2, '0')}Z`,
        lastEditedAt: null,
        url: 'u',
        state: 'SUBMITTED',
        reviewId: 'R',
        replyTo: null,
        isMinimized: false,
        minimizedReason: null,
        viewerCanUpdate: false,
        viewerCanDelete: false,
        viewerDidAuthor: false,
      },
    ],
    ...over,
  }
}
