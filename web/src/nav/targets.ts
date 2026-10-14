// Moving a place between the two sides of a diff: a line of the base file (the original model)
// and the line of the head file (the modified model) that shows it. The unified view has only
// the modified editor, so a place on the base side lands there.
import type { EditorSide, LineChange } from '../threads'

/**
 * Maps a line of the `from` side of a diff to the other side. A line inside a change maps to
 * the change's first line on the other side (for a change that removes everything there, the
 * line before the removal, or line 1); a line outside the changes is shifted by their sizes.
 * Like Monaco's `ILineChange`, a side's range is empty (end 0) for a pure insertion or removal,
 * and then its start is the line the other side's lines come after (0: the top of the file).
 * `changes` are ordered by position, as the diff computer gives them.
 */
export function mapLine(changes: readonly LineChange[], line: number, from: EditorSide): number {
  const fromOriginal = from === 'original'
  let delta = 0
  for (const c of changes) {
    const start = fromOriginal ? c.originalStartLineNumber : c.modifiedStartLineNumber
    const end = fromOriginal ? c.originalEndLineNumber : c.modifiedEndLineNumber
    const otherStart = fromOriginal ? c.modifiedStartLineNumber : c.originalStartLineNumber
    const otherEnd = fromOriginal ? c.modifiedEndLineNumber : c.originalEndLineNumber
    if (end !== 0 && line >= start && line <= end) return Math.max(1, otherStart)
    // Last line the change covers on each side, or the line an insertion or removal follows.
    const last = end === 0 ? start : end
    if (line <= last) break
    delta = (otherEnd === 0 ? otherStart : otherEnd) - last
  }
  return line + delta
}
