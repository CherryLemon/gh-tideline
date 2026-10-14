// Moving a place between the two sides of a diff: a line of the base file (the original model)
// and the line of the head file (the modified model) that shows it. The unified view has only
// the modified editor, so a place on the base side lands there. And finding the file of the pull
// request that shows a place the index found.
import type { ChangedFile, NavSide } from '../api'
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

/** A file to open: its path as `?file=` names it, and the side of it to show. */
export interface Target {
  path: string
  side: NavSide
}

/**
 * The file that shows `path` as the commit of `side` has it (the index knows the paths of a commit, not of the pull
 * request): the changed file whose version at that side it is (a renamed file's old path is on its base side; a copy
 * doesn't stand for the file it was copied from), else an unchanged file, which reads the same on both sides and is
 * shown at the head. Null when the pull request has no such file. `tree`: the paths of the head tree, or null while
 * they aren't listed, when an unchanged file is taken to be there.
 */
export function resolveTarget(path: string, side: NavSide, files: readonly ChangedFile[], tree: { has(path: string): boolean } | null): Target | null {
  if (side === 'base') {
    const old = files.find((f) => f.oldOid !== null && f.status !== 'C' && (f.status === 'R' ? f.oldPath : f.path) === path)
    if (old) return { path: old.path, side: 'base' }
    // A file put at this path since is not the one the base had.
    if (files.some((f) => f.path === path && f.newOid !== null)) return null
  } else if (files.some((f) => f.path === path && f.newOid !== null)) {
    return { path, side: 'head' }
  }
  return tree === null || tree.has(path) ? { path, side: 'head' } : null
}
