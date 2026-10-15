import type * as Monaco from 'monaco-editor/editor/editor.api'
import { describe, expect, it, vi } from 'vitest'
import { modelUri, parseModelUri } from './files'

// `files.ts` reaches Monaco and its workers, which a unit test neither needs nor can load: a URI is its parts here.
vi.mock('./setup', () => ({ monaco: { Uri: { from: (parts: object) => ({ ...parts }) } } }))
vi.mock('./language', () => ({ languageForPath: () => 'plaintext' }))

const OID = '00c45b882c0626f35b6b0ae4b1c06091dcd872e9'

const uri = (scheme: string, authority: string, path: string) => ({ scheme, authority, path }) as unknown as Monaco.Uri

const PATHS = [
  'src/app/main.ts',
  'README.md',
  'a b/c d.ts',
  'dir/#1 notes.md',
  '100%/done%20.txt',
  'src/ünï/日本語.ts',
  'a?b=c&d.ts',
  'x/../y.ts',
  '.hidden/.f',
  'a\\b.ts',
  'src/[id]/(group)/page.tsx',
]

describe('parseModelUri', () => {
  it('reads the side, the blob and the path of a model', () => {
    expect(parseModelUri(uri('dr', 'head', `/${OID}/src/app/main.ts`))).toEqual({ side: 'head', oid: OID, path: 'src/app/main.ts' })
    expect(parseModelUri(uri('dr', 'base', `/${OID}/README.md`))).toEqual({ side: 'base', oid: OID, path: 'README.md' })
  })

  it('has no blob for a side the file does not have', () => {
    expect(parseModelUri(uri('dr', 'base', '/none/src/new.ts'))).toEqual({ side: 'base', oid: null, path: 'src/new.ts' })
  })

  it('keeps a path as it is, whatever characters it has (spaces, #, %, other scripts)', () => {
    for (const path of PATHS) {
      expect(parseModelUri(uri('dr', 'head', `/${OID}/${path}`))).toEqual({ side: 'head', oid: OID, path })
      expect(parseModelUri(uri('dr', 'base', `/none/${path}`))).toEqual({ side: 'base', oid: null, path })
    }
  })

  it('is the inverse of modelUri', () => {
    for (const path of PATHS) {
      expect(parseModelUri(modelUri('base', OID, path))).toEqual({ side: 'base', oid: OID, path })
      expect(parseModelUri(modelUri('head', null, path))).toEqual({ side: 'head', oid: null, path })
    }
  })

  it('refuses a URI that is not one of our models', () => {
    expect(parseModelUri(uri('file', 'head', `/${OID}/a.ts`))).toBeNull()
    expect(parseModelUri(uri('dr', 'other', `/${OID}/a.ts`))).toBeNull()
    expect(parseModelUri(uri('dr', '', `/${OID}/a.ts`))).toBeNull()
  })

  it('refuses a path that does not start with a blob and name a file', () => {
    expect(parseModelUri(uri('dr', 'head', `/${OID.slice(1)}/a.ts`))).toBeNull()
    expect(parseModelUri(uri('dr', 'head', `/${OID}0/a.ts`))).toBeNull()
    expect(parseModelUri(uri('dr', 'head', `/${OID.toUpperCase()}/a.ts`))).toBeNull()
    expect(parseModelUri(uri('dr', 'head', `/${OID}`))).toBeNull()
    expect(parseModelUri(uri('dr', 'head', `/${OID}/`))).toBeNull()
    expect(parseModelUri(uri('dr', 'head', '/none'))).toBeNull()
    expect(parseModelUri(uri('dr', 'head', `${OID}/a.ts`))).toBeNull()
    expect(parseModelUri(uri('dr', 'head', '/untitled/a.ts'))).toBeNull()
  })
})

describe('modelUri', () => {
  it('puts the blob, or none, before the path', () => {
    expect(modelUri('head', OID, 'src/a.ts')).toEqual({ scheme: 'dr', authority: 'head', path: `/${OID}/src/a.ts` })
    expect(modelUri('base', null, 'src/a.ts')).toEqual({ scheme: 'dr', authority: 'base', path: '/none/src/a.ts' })
  })
})
