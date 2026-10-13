// Where review threads go: under a line of the diff (a view zone) or in the panel above it.
// GitHub gives each line thread a line in the whole file on one side, at the threads' head:
// RIGHT lines are lines of the head file (the modified model), LEFT lines of the merge-base
// file (the original model). The diff itself maps original lines into the modified editor:
// for unified view, and for unfolding (only the modified editor can unfold regions).
import type { ChangedFile, ReviewThread } from './api'

/** The part of Monaco's `ILineChange` placement needs (pure insertions/deletions use end 0). */
export interface LineChange {
  originalStartLineNumber: number
  originalEndLineNumber: number
  modifiedStartLineNumber: number
  modifiedEndLineNumber: number
}

export type EditorSide = 'original' | 'modified'

/** Ordinals order zones that share a line; Monaco's own diff zones use the default, 10000. */
const ORDINAL_UNDER_LINE = 5000
const ORDINAL_AFTER_DELETED = 20000

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

/** Where an original line shows up in the modified editor (unified view). */
export function origToMod(changes: readonly LineChange[], line: number): { line: number; deleted: boolean } {
  let delta = 0
  for (const c of changes) {
    const removed = c.originalEndLineNumber !== 0
    if (removed && line >= c.originalStartLineNumber && line <= c.originalEndLineNumber) {
      // Monaco draws deleted lines in a zone above the change's first modified line.
      const after = c.modifiedEndLineNumber === 0 ? c.modifiedStartLineNumber : c.modifiedStartLineNumber - 1
      return { line: after, deleted: true }
    }
    // Last original line the change covers, or the line an insertion follows.
    const origEnd = removed ? c.originalEndLineNumber : c.originalStartLineNumber
    const modEnd = c.modifiedEndLineNumber === 0 ? c.modifiedStartLineNumber : c.modifiedEndLineNumber
    if (line <= origEnd) break
    delta = modEnd - origEnd
  }
  return { line: line + delta, deleted: false }
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
    const onOriginal = side === 'LEFT'
    if (line < 1 || line > (onOriginal ? lineCounts.original : lineCounts.modified)) return toPanel('out-of-range')
    if (lineChanges === null) return
    const start = thread.startLine !== null && (thread.startSide ?? side) === side ? thread.startLine : null
    const highlight = start === null ? null : { from: start, to: line }
    if (!onOriginal) {
      zones.push({ thread, editor: 'modified', afterLine: line, ordinal: ORDINAL_UNDER_LINE + i, highlight, reveal: line })
      return
    }
    const at = origToMod(lineChanges, line)
    const reveal = at.line > 0 ? at.line : null
    if (view === 'split') {
      zones.push({ thread, editor: 'original', afterLine: line, ordinal: ORDINAL_UNDER_LINE + i, highlight, reveal })
      return
    }
    // Unified view: the original side lives inside the modified editor.
    zones.push({
      thread,
      editor: 'modified',
      afterLine: at.line,
      ordinal: (at.deleted ? ORDINAL_AFTER_DELETED : ORDINAL_UNDER_LINE) + i,
      highlight: null,
      reveal,
    })
  })
  return { zones, panel }
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
