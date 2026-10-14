// What the lists of the declarations and of the usages of a name say around their rows. Pure, so that the wording can be tested.
import type { NavResult } from '../api'

/**
 * The title and the note under the rows of the declarations to choose from. A list the server cut short (`truncated`) has
 * as many hits as it was allowed to return: the count is "200+" and the note says that the rest is not listed.
 */
export function declarationsText(name: string, result: Pick<NavResult, 'hits' | 'truncated'>): { title: string; note?: string } {
  const n = result.hits.length
  const title = `Choose declaration of ${name} (${result.truncated ? `${n}+` : n})`
  return result.truncated ? { title, note: `Only the first ${n} declarations are listed` } : { title }
}

/**
 * The title, the text for a name nothing uses and the note under the rows. A list the server cut short (`truncated`) has
 * as many hits as it was allowed to return: the count is "1000+" and the note says that the rest is not listed.
 */
export function usagesText(name: string, result: Pick<NavResult, 'hits' | 'truncated'>): { title: string; empty: string; note?: string } {
  const n = result.hits.length
  const text = {
    title: `Usages of ${name} · matched by name (${result.truncated ? `${n}+` : n})`,
    empty: 'No usages found',
  }
  // Matching names is no analysis of the code: where it leaves a use out, or lists another thing of the same name, this says why.
  if (n === 0) return { ...text, note: "Usages are matched by name in this language's files" }
  return result.truncated ? { ...text, note: `Only the first ${n} usages are listed` } : text
}
