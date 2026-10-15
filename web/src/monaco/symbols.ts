// The outline of a model for Monaco, from the server's symbols: it is what puts the enclosing symbols (breadcrumbs) in the
// header of a collapsed stretch of unchanged lines in the diff.
import type * as Monaco from 'monaco-editor/editor/editor.api'
import type { RepoRef } from '../api'
import { getSymbols, nestSymbols, type SymbolNode } from '../nav/symbols'
import { parseModelUri } from './files'
import { monaco } from './setup'

const { SymbolKind } = monaco.languages

const KINDS: Record<string, Monaco.languages.SymbolKind> = {
  function: SymbolKind.Function,
  method: SymbolKind.Method,
  class: SymbolKind.Class,
  interface: SymbolKind.Interface,
  module: SymbolKind.Module,
  namespace: SymbolKind.Namespace,
  macro: SymbolKind.Function,
  type: SymbolKind.Class,
  enum: SymbolKind.Enum,
  constant: SymbolKind.Constant,
  field: SymbolKind.Field,
  struct: SymbolKind.Struct,
  trait: SymbolKind.Interface,
  implementation: SymbolKind.Class,
  variable: SymbolKind.Variable,
  property: SymbolKind.Property,
  constructor: SymbolKind.Constructor,
}

let repo: RepoRef | null = null

/** The repository the models belong to; null while there is none. */
export function setSymbolsRepo(r: RepoRef | null): void {
  repo = r
}

function toDocumentSymbol(node: SymbolNode, model: Monaco.editor.ITextModel): Monaco.languages.DocumentSymbol {
  const s = node.symbol
  const lines = model.getLineCount()
  const at = (line: number) => Math.min(Math.max(1, line), lines)
  const endLine = at(s.endLine)
  return {
    name: s.name,
    detail: '',
    kind: KINDS[s.kind] ?? SymbolKind.Variable,
    tags: [],
    range: { startLineNumber: at(s.startLine), startColumn: 1, endLineNumber: endLine, endColumn: model.getLineMaxColumn(endLine) },
    selectionRange: { startLineNumber: at(s.line), startColumn: s.column, endLineNumber: at(s.line), endColumn: s.endColumn },
    children: node.children.map((c) => toDocumentSymbol(c, model)),
  }
}

// Only for our models (`dr:` URIs): a file's blob oid and path are in the URI.
monaco.languages.registerDocumentSymbolProvider(
  { scheme: 'dr' },
  {
    displayName: 'Tideline',
    async provideDocumentSymbols(model) {
      const target = parseModelUri(model.uri)
      if (!repo || !target?.oid || model.getLanguageId() === 'plaintext') return []
      try {
        const { symbols } = await getSymbols(repo, target.oid, target.path)
        return model.isDisposed() ? [] : nestSymbols(symbols).map((n) => toDocumentSymbol(n, model))
      } catch {
        return []
      }
    },
  },
)
