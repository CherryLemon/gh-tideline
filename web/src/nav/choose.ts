// What a click on a name leads to, given the definitions the index found for it. Pure, so that the cases can be tested.
import type { NavHit, NavResult, NavSide, SymbolInfo } from '../api'
import type { Identifier } from './identifier'
import { definitionAt } from './symbols'

/** The name clicked: its place in `path`, the file as that side's commit has it (a renamed file has its old path on the base side). */
export interface Click {
  path: string
  side: NavSide
  line: number
  /** Where the name starts. */
  column: number
  name: string
}

/**
 * The click on a name found in `path`. Wherever on the name it landed (also on the right half of its last character,
 * which Monaco reports as the column after the name), it is at the name's start: where a definition's name begins.
 */
export function clickOn(path: string, side: NavSide, id: Identifier): Click {
  return { path, side, line: id.line, column: id.nameColumn, name: id.name }
}

/**
 * - usages: the click is on the name of a definition, so the definition is no place to go to; its usages are the answer;
 * - none: the index knows no definition of the name;
 * - jump: one place to go to, or only one in the file clicked in;
 * - popup: several, to choose from, nearest first as the server ranked them.
 */
export type Choice = { kind: 'usages' } | { kind: 'none' } | { kind: 'jump'; hit: NavHit } | { kind: 'popup'; hits: NavHit[] }

/** `symbols`: those of the file clicked in, at the version shown (empty when they are not known). */
export function choose(click: Click, symbols: readonly SymbolInfo[], result: NavResult): Choice {
  const onHit = result.hits.some((h) => h.path === click.path && h.line === click.line && h.column <= click.column && click.column < h.endColumn)
  if (definitionAt(symbols, click.line, click.column) || onHit) return { kind: 'usages' }
  if (result.hits.length === 0) return { kind: 'none' }
  if (result.hits.length === 1) return { kind: 'jump', hit: result.hits[0] }
  const here = result.hits.filter((h) => h.path === click.path)
  return here.length === 1 ? { kind: 'jump', hit: here[0] } : { kind: 'popup', hits: result.hits }
}
