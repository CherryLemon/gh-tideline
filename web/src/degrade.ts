// How a changed file is displayed: the degradation rules of design.md §5.2 and the
// image-type decisions behind design.md §8. Pure, no DOM or Monaco.
import type { ChangedFile } from './api'

export const LIMITS = {
  /** Above this, syntax highlighting is turned off. */
  plainBytes: 1 << 20,
  /** Above this (or deferLines), the file is not loaded until the user asks. */
  deferBytes: 5 << 20,
  deferLines: 50_000,
  /** A line longer than this (minified code) turns highlighting off. */
  longLine: 10_000,
  /** Monaco diff limits aligned with the thresholds above. */
  maxComputationTimeMs: 5000,
  maxFileSizeMb: 5,
}

export interface TextMeta {
  bytes: number
  lines: number
  maxLine: number
}

export function measure(text: string, bytes: number): TextMeta {
  let lines = 1
  let maxLine = 0
  let start = 0
  for (;;) {
    const nl = text.indexOf('\n', start)
    const end = nl < 0 ? text.length : nl
    if (end - start > maxLine) maxLine = end - start
    if (nl < 0) break
    lines++
    start = nl + 1
  }
  return { bytes, lines, maxLine }
}

/** One side of a text file: measured contents, or only a byte count when it was too large to download. */
export type SideMeasure = { meta: TextMeta } | { tooLargeBytes: number } | null

export const PLAIN_REASONS = {
  forced: 'Over 5 MB or 50,000 lines: syntax highlighting off',
  bytes: 'Over 1 MB: syntax highlighting off',
  longLine: 'Line over 10,000 characters (likely minified): syntax highlighting off',
} as const

/**
 * Decide how to show a text file from its two sides (null = side absent).
 * Files over the defer limits are only shown when forced, and then always as plain text.
 */
export function decideText(sides: SideMeasure[], force: boolean): { kind: 'deferred' } | { kind: 'text'; plainReason: string | null } {
  const metas = sides.flatMap((s) => (s && 'meta' in s ? [s.meta] : []))
  const tooBig =
    sides.some((s) => s !== null && 'tooLargeBytes' in s) ||
    metas.some((m) => m.bytes > LIMITS.deferBytes || m.lines > LIMITS.deferLines)
  if (tooBig && !force) return { kind: 'deferred' }
  let plainReason: string | null = null
  if (tooBig) plainReason = PLAIN_REASONS.forced
  else if (metas.some((m) => m.bytes > LIMITS.plainBytes)) plainReason = PLAIN_REASONS.bytes
  else if (metas.some((m) => m.maxLine > LIMITS.longLine)) plainReason = PLAIN_REASONS.longLine
  return { kind: 'text', plainReason }
}

/** Git mode of a submodule entry (a "gitlink": the oid is a commit in another repository). */
export const GITLINK_MODE = '160000'

export function isGitlink(mode: string): boolean {
  return mode === GITLINK_MODE
}

/**
 * Raster formats a browser can only ever render as a picture. Only these may be wrapped in a
 * same-origin `blob:` URL: opened in a tab, such a URL still can't run script (design.md §8).
 */
const RASTER_MIME: Record<string, string> = {
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  gif: 'image/gif',
  webp: 'image/webp',
  avif: 'image/avif',
  bmp: 'image/bmp',
  ico: 'image/x-icon',
}

const SAFE_BLOB_MIMES = new Set(Object.values(RASTER_MIME))

export type ImageType = { kind: 'raster'; mime: string } | { kind: 'svg' }

/** How a binary file can be shown as an image, from its extension; null when it isn't one. */
export function imageType(path: string): ImageType | null {
  const name = path.toLowerCase()
  const dot = name.lastIndexOf('.')
  if (dot < 0) return null
  const ext = name.slice(dot + 1)
  if (ext === 'svg') return { kind: 'svg' }
  const mime = RASTER_MIME[ext]
  return mime ? { kind: 'raster', mime } : null
}

/** Whether repository bytes of this type may be exposed through a same-origin `blob:` URL. */
export function isSafeBlobMime(mime: string): boolean {
  return SAFE_BLOB_MIMES.has(mime)
}

export type FileDisplay = 'submodule' | 'image' | 'binary' | 'text'

/**
 * What the diff pane shows for a file. Submodules never fetch blobs: their oids are commits.
 * Text SVGs (git didn't flag them binary) are diffed as text, never rendered.
 */
export function fileDisplay(file: Pick<ChangedFile, 'path' | 'binary' | 'oldMode' | 'newMode'>): FileDisplay {
  if (isGitlink(file.oldMode) || isGitlink(file.newMode)) return 'submodule'
  if (file.binary) return imageType(file.path) ? 'image' : 'binary'
  return 'text'
}

/** How many leading bytes `looksBinary` inspects (git's own heuristic uses the same window). */
export const SNIFF_BYTES = 8000

/** Git's binary heuristic for a blob it has no diff verdict for: a NUL byte near the start. */
export function looksBinary(bytes: Uint8Array): boolean {
  return bytes.subarray(0, SNIFF_BYTES).includes(0)
}

export function shortSha(oid: string): string {
  return oid.slice(0, 7)
}
