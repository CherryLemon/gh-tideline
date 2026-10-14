// Where review threads go: under a line of the diff (a view zone) or in the panel above it.
// GitHub gives each line thread a line in the whole file on one side, at the threads' head:
// RIGHT lines are lines of the head file (the modified model), LEFT lines of the merge-base
// file (the original model). The diff itself maps original lines into the modified editor:
// for unified view, and for unfolding (only the modified editor can unfold regions).
import type { ChangedFile, DiffSide, ReviewThread } from './api'

/** The part of Monaco's `ILineChange` placement needs (pure insertions/deletions use end 0). */
export interface LineChange {
  originalStartLineNumber: number
  originalEndLineNumber: number
  modifiedStartLineNumber: number
  modifiedEndLineNumber: number
}

export type EditorSide = 'original' | 'modified'

/**
 * Ordinals order zones that share a line. Monaco's own diff zones use the default, 10000, so
 * cards come first: right under their line, and above the removed lines Monaco draws in the
 * modified editor (unified view).
 */
const ORDINAL_UNDER_LINE = 5000
/** Boxes for new comments come after the threads on their line. */
const ORDINAL_DRAFT = 9000

export interface ThreadZone {
  thread: ReviewThread
  editor: EditorSide
  /** The zone sits below this line (0: above the first line). */
  afterLine: number
  ordinal: number
  /** Lines of a multi-line thread to highlight, when they are all in `editor`. */
  highlight: { from: number; to: number } | null
  /** Line of the modified editor that must not stay folded away, so the zone shows. */
  reveal: number | null
}

/**
 * Why a thread is in the panel instead of the diff:
 * - file: a comment on the whole file;
 * - outdated: its line is gone at the threads' head;
 * - head: the threads were read at another head than the one shown, so lines may be off;
 * - no-diff: the file isn't shown as a text diff (image, binary, too large, unchanged...);
 * - old-path: a RIGHT thread stored under the old path of a renamed file;
 * - out-of-range: the line is past the end of the file as shown.
 */
export type PanelReason = 'file' | 'outdated' | 'head' | 'no-diff' | 'old-path' | 'out-of-range'

export interface PanelItem {
  thread: ReviewThread
  reason: PanelReason
}

export interface Placement {
  zones: ThreadZone[]
  panel: PanelItem[]
}

export interface PlaceInput {
  /** The file's threads (`threadsForFile`). */
  threads: readonly ReviewThread[]
  /** Renamed files: threads stored under this path refer to it on the LEFT. */
  oldPath?: string
  /** How the text diff is shown; null when the file isn't shown as a text diff. */
  view: 'split' | 'unified' | null
  /** The diff as shown; until it is computed, line threads are left out (they come back). */
  lineChanges: readonly LineChange[] | null
  lineCounts: { original: number; modified: number }
  /** Whether the threads were read at the head the page shows. */
  headOk: boolean
}

export interface ModLine {
  /** The modified line it shows up at; for a removed line, the one Monaco draws it after (0: the top). */
  line: number
  deleted: boolean
  /** For a removed line: the last line its change adds, if it adds any. */
  lastAdded: number | null
}

/** Where an original line shows up in the modified editor (unified view). */
export function origToMod(changes: readonly LineChange[], line: number): ModLine {
  let delta = 0
  for (const c of changes) {
    const removed = c.originalEndLineNumber !== 0
    if (removed && line >= c.originalStartLineNumber && line <= c.originalEndLineNumber) {
      // Monaco draws removed lines in a zone above the change's first modified line.
      const added = c.modifiedEndLineNumber !== 0
      return { line: added ? c.modifiedStartLineNumber - 1 : c.modifiedStartLineNumber, deleted: true, lastAdded: added ? c.modifiedEndLineNumber : null }
    }
    // Last original line the change covers, or the line an insertion follows.
    const origEnd = removed ? c.originalEndLineNumber : c.originalStartLineNumber
    const modEnd = c.modifiedEndLineNumber === 0 ? c.modifiedStartLineNumber : c.modifiedEndLineNumber
    if (line <= origEnd) break
    delta = modEnd - origEnd
  }
  return { line: line + delta, deleted: false, lastAdded: null }
}

function firstCommentAt(t: ReviewThread): string {
  return t.comments[0]?.createdAt ?? ''
}

/** Oldest first, so cards on the same line keep their order across refreshes. */
function byAge(a: ReviewThread, b: ReviewThread): number {
  const at = firstCommentAt(a)
  const bt = firstCommentAt(b)
  if (at !== bt) return at < bt ? -1 : 1
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0
}

/** Lines on one side: a thread's, or those a new comment is about. */
export interface LineAnchor {
  side: DiffSide
  line: number
  /** First line of a range on the same side; null for one line. */
  startLine: number | null
}

/** Where a zone for `anchor` goes in the diff as shown (see `PlaceInput`). */
interface Spot {
  editor: EditorSide
  afterLine: number
  highlight: { from: number; to: number } | null
  reveal: number | null
}

function spotFor(anchor: LineAnchor, view: 'split' | 'unified', lineChanges: readonly LineChange[]): Spot {
  const { line, side } = anchor
  const highlight = anchor.startLine === null ? null : { from: anchor.startLine, to: line }
  if (side === 'RIGHT') return { editor: 'modified', afterLine: line, highlight, reveal: line }
  const at = origToMod(lineChanges, line)
  if (view === 'split') return { editor: 'original', afterLine: line, highlight, reveal: at.line > 0 ? at.line : null }
  // Unified view: the original side lives inside the modified editor. Monaco lines up the
  // original editor (the old line numbers there) by the line each zone follows, so a card
  // between the removed lines and the added ones would push the old numbers out of line: a
  // removed line's card goes after the lines its change adds, or above the removed lines
  // when it adds none.
  const after = at.lastAdded ?? at.line
  return { editor: 'modified', afterLine: after, highlight: null, reveal: after > 0 ? after : null }
}

function lineCount(counts: PlaceInput['lineCounts'], side: DiffSide): number {
  return side === 'LEFT' ? counts.original : counts.modified
}

export function placeThreads(input: PlaceInput): Placement {
  const { view, lineChanges, lineCounts, headOk, oldPath } = input
  const zones: ThreadZone[] = []
  const panel: PanelItem[] = []
  const threads = [...input.threads].sort(byAge)
  threads.forEach((thread, i) => {
    const toPanel = (reason: PanelReason) => panel.push({ thread, reason })
    if (thread.subjectType === 'FILE') return toPanel('file')
    const { line, side } = thread
    if (line === null || side === null) return toPanel('outdated')
    if (!headOk) return toPanel('head')
    if (view === null) return toPanel('no-diff')
    if (oldPath !== undefined && thread.path === oldPath && side !== 'LEFT') return toPanel('old-path')
    if (line < 1 || line > lineCount(lineCounts, side)) return toPanel('out-of-range')
    if (lineChanges === null) return
    const startLine = thread.startLine !== null && (thread.startSide ?? side) === side ? thread.startLine : null
    zones.push({ thread, ordinal: ORDINAL_UNDER_LINE + i, ...spotFor({ side, line, startLine }, view, lineChanges) })
  })
  return { zones, panel }
}

/** An open box for a new comment on lines of the file. */
export interface LineDraft extends LineAnchor {
  /** Its draft key (`draftKey`). */
  key: string
}

export interface DraftZone extends Spot {
  draft: LineDraft
  ordinal: number
}

/**
 * Why a box is listed above the diff instead of under its lines (the file isn't shown as a
 * text diff, or the lines are past its end).
 */
export type DraftPanelReason = 'no-diff' | 'out-of-range'

export interface DraftPlacement {
  zones: DraftZone[]
  panel: { draft: LineDraft; reason: DraftPanelReason }[]
}

/**
 * Boxes for new comments go under their last line, after the threads there (and before
 * Monaco's own zones); like threads, they wait while the diff is computed.
 */
export function placeDrafts(input: Omit<PlaceInput, 'threads' | 'oldPath' | 'headOk'> & { drafts: readonly LineDraft[] }): DraftPlacement {
  const { view, lineChanges, lineCounts } = input
  const out: DraftPlacement = { zones: [], panel: [] }
  input.drafts.forEach((draft, i) => {
    if (view === null) return out.panel.push({ draft, reason: 'no-diff' })
    const last = lineCount(lineCounts, draft.side)
    if (draft.line < 1 || draft.line > last || (draft.startLine !== null && draft.startLine < 1)) {
      return out.panel.push({ draft, reason: 'out-of-range' })
    }
    if (lineChanges === null) return
    out.zones.push({ draft, ordinal: ORDINAL_DRAFT + i, ...spotFor(draft, view, lineChanges) })
  })
  return out
}

/** Lines of context git (and so GitHub) shows around a change. */
const CONTEXT = 3

/** Lines `from` to `to`, both included. */
export interface LineSpan {
  from: number
  to: number
}

/**
 * The lines of one side GitHub takes comments on: those of the diff's hunks, so the changed
 * lines and 3 lines around them (S12). Worked out from Monaco's diff, which can differ from
 * GitHub's (and ignores whitespace when asked to): GitHub has the last word. `lineCount` is
 * 0 for a side the file doesn't have (added or deleted).
 */
export function commentableLines(changes: readonly LineChange[], side: EditorSide, lineCount: number): LineSpan[] {
  const spans: LineSpan[] = []
  for (const c of changes) {
    const start = side === 'original' ? c.originalStartLineNumber : c.modifiedStartLineNumber
    const end = side === 'original' ? c.originalEndLineNumber : c.modifiedEndLineNumber
    // Nothing on this side: the change sits between `start` and the line after it.
    const from = end === 0 ? start + 1 - CONTEXT : start - CONTEXT
    const to = (end === 0 ? start : end) + CONTEXT
    const span = { from: Math.max(1, from), to: Math.min(lineCount, to) }
    if (span.from > span.to) continue
    const prev = spans[spans.length - 1]
    if (prev && span.from <= prev.to + 1) prev.to = Math.max(prev.to, span.to)
    else spans.push(span)
  }
  return spans
}

/**
 * Lines git (and so GitHub) counts in a text Monaco shows as `lineCount` lines, the last
 * `lastLineLength` long: a final newline ends the last line rather than starting an empty one,
 * and an empty text has none.
 */
export function gitLineCount(lineCount: number, lastLineLength: number): number {
  return lastLineLength === 0 ? lineCount - 1 : lineCount
}

export function inSpans(spans: readonly LineSpan[], line: number): boolean {
  return spans.some((s) => line >= s.from && line <= s.to)
}

function renamedFrom(file: ChangedFile): string | undefined {
  return file.oldPath !== undefined && file.oldPath !== file.path ? file.oldPath : undefined
}

/** The threads of a changed file, including LEFT threads stored under a renamed file's old path. */
export function threadsForFile(threads: readonly ReviewThread[], file: ChangedFile): ReviewThread[] {
  const old = renamedFrom(file)
  return threads.filter((t) => t.path === file.path || (old !== undefined && t.path === old))
}

export interface ThreadCounts {
  threads: number
  unresolved: number
}

/**
 * Thread counts per file path; threads under a renamed file's old path count for the file.
 * Threads of paths outside `files` count under their own path.
 */
export function fileCounts(threads: readonly ReviewThread[], files: readonly ChangedFile[]): Map<string, ThreadCounts> {
  const alias = new Map<string, string>()
  for (const f of files) {
    const old = renamedFrom(f)
    if (old !== undefined) alias.set(old, f.path)
  }
  for (const f of files) alias.delete(f.path)
  const out = new Map<string, ThreadCounts>()
  for (const t of threads) {
    const path = alias.get(t.path) ?? t.path
    const c = out.get(path) ?? { threads: 0, unresolved: 0 }
    c.threads++
    if (!t.isResolved) c.unresolved++
    out.set(path, c)
  }
  return out
}

export interface HunkLine {
  kind: 'add' | 'del' | 'ctx'
  text: string
}

/**
 * The end of the diff hunk GitHub stored with a thread (it ends at the commented line), for
 * showing outdated threads; at most `max` lines.
 */
export function hunkOf(thread: ReviewThread, max = 4): HunkLine[] {
  const lines: HunkLine[] = []
  for (const raw of thread.diffHunk.split('\n')) {
    if (raw === '' || raw.startsWith('@@') || raw.startsWith('\\')) continue
    const mark = raw[0]
    const kind = mark === '+' ? 'add' : mark === '-' ? 'del' : 'ctx'
    lines.push({ kind, text: raw.slice(1) })
  }
  return lines.slice(-max)
}
