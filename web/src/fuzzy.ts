// Subsequence fuzzy matching for the file finder. Matches in the file name rank above
// matches spread over directories; consecutive characters and word starts score higher.

export interface FuzzyMatch {
  score: number
  /** Indices into the matched string, for highlighting. */
  positions: number[]
}

const SEPARATORS = new Set(['/', '.', '_', '-', ' '])

function matchFrom(text: string, query: string, start: number): FuzzyMatch | null {
  const lower = text.toLowerCase()
  const positions: number[] = []
  let score = 0
  let ti = start
  let prev = -2
  for (let qi = 0; qi < query.length; qi++) {
    const ch = query[qi]
    const found = lower.indexOf(ch, ti)
    if (found < 0) return null
    let s = 1
    if (found === prev + 1) s += 5
    if (found === 0 || SEPARATORS.has(text[found - 1])) s += 4
    else if (text[found] !== lower[found] && text[found - 1] === lower[found - 1]) s += 3 // camelCase hump
    score += s
    positions.push(found)
    prev = found
    ti = found + 1
  }
  return { score, positions }
}

export function fuzzyMatch(path: string, rawQuery: string): FuzzyMatch | null {
  const query = rawQuery.replace(/\s+/g, '').toLowerCase()
  if (!query) return { score: 0, positions: [] }
  const nameStart = path.lastIndexOf('/') + 1
  // Prefer a match entirely inside the file name.
  const inName = matchFrom(path, query, nameStart)
  if (inName) {
    let score = inName.score + 20
    if (path.slice(nameStart).toLowerCase().startsWith(query)) score += 15
    return { score: score - path.length * 0.01, positions: inName.positions }
  }
  const anywhere = matchFrom(path, query, 0)
  if (!anywhere) return null
  return { score: anywhere.score - path.length * 0.01, positions: anywhere.positions }
}
