// Loading file contents for the diff view: blob fetching, degradation rules and the
// model LRU (design.md §5.2, §8).
import type * as Monaco from 'monaco-editor/editor/editor.api'
import { getBlob, type ChangedFile, type RepoRef, type TreeEntry } from '../api'
import {
  LIMITS,
  SNIFF_BYTES,
  decideText,
  fileDisplay,
  imageType,
  isGitlink,
  isSafeBlobMime,
  looksBinary,
  measure,
  type ImageType,
  type SideMeasure,
  type TextMeta,
} from '../degrade'
import { PinnedLru } from '../lru'
import { monaco } from './setup'
import { languageForPath } from './language'

const MODEL_CACHE_SIZE = 50

export type Side = 'base' | 'head'

/**
 * Monaco models keyed by `dr://{side}/{oid}/{path}`. Models shown in the editor (setInUse) and
 * models a pending load has obtained but not attached yet (pinned) are never evicted.
 */
export const models = new PinnedLru<Monaco.editor.ITextModel>(
  MODEL_CACHE_SIZE,
  (m) => m.dispose(),
  (m) => !m.isDisposed(),
)

const metaOf = new WeakMap<Monaco.editor.ITextModel, TextMeta>()

function modelMeta(m: Monaco.editor.ITextModel): TextMeta {
  return metaOf.get(m) ?? { bytes: m.getValueLength(), lines: m.getLineCount(), maxLine: 0 }
}

const decoder = new TextDecoder('utf-8', { fatal: false })

export function modelUri(side: Side, oid: string | null, path: string): Monaco.Uri {
  return monaco.Uri.from({ scheme: 'dr', authority: side, path: `/${oid ?? 'none'}/${path}` })
}

type SideText = { kind: 'text'; text: string; meta: TextMeta } | { kind: 'tooLarge'; bytes: number }

const inflight = new Map<string, Promise<SideText>>()

function contentLength(res: Response): number | null {
  const v = res.headers.get('content-length')
  if (v === null) return null
  const n = Number(v)
  return Number.isFinite(n) ? n : null
}

function fetchSideText(repo: RepoRef, oid: string, force: boolean): Promise<SideText> {
  const key = `${repo.host}/${repo.owner}/${repo.repo}/${oid}/${force}`
  const pending = inflight.get(key)
  if (pending) return pending
  const p = (async (): Promise<SideText> => {
    const res = await getBlob(repo, oid)
    const len = contentLength(res)
    if (!force && len !== null && len > LIMITS.deferBytes) {
      await res.body?.cancel()
      return { kind: 'tooLarge', bytes: len }
    }
    const buf = await res.arrayBuffer()
    if (!force && buf.byteLength > LIMITS.deferBytes) return { kind: 'tooLarge', bytes: buf.byteLength }
    const text = decoder.decode(buf)
    return { kind: 'text', text, meta: measure(text, buf.byteLength) }
  })()
  inflight.set(key, p)
  const clear = () => inflight.delete(key)
  p.then(clear, clear)
  return p
}

/** Byte size of a blob from its Content-Length, without downloading the body. */
async function blobSize(repo: RepoRef, oid: string | null): Promise<number | null> {
  if (!oid) return null
  const res = await getBlob(repo, oid)
  const len = contentLength(res)
  if (len !== null) {
    await res.body?.cancel()
    return len
  }
  return (await res.arrayBuffer()).byteLength
}

export interface SideInfo {
  bytes: number | null
  lines?: number
}

export type LoadedFile =
  | {
      kind: 'text'
      original: Monaco.editor.ITextModel
      modified: Monaco.editor.ITextModel
      /** Highlighting disabled because of size or very long lines. */
      plainReason: string | null
      forced: boolean
      /**
       * Unpin the models. Call exactly once: after attaching them (and marking them in use),
       * or when the result is abandoned.
       */
      release(): void
    }
  | {
      /** An unchanged file: its head content alone, no diff. */
      kind: 'single'
      model: Monaco.editor.ITextModel
      plainReason: string | null
      forced: boolean
      /** As for `text`. */
      release(): void
    }
  | { kind: 'deferred'; base: SideInfo; head: SideInfo }
  | { kind: 'binary'; base: SideInfo; head: SideInfo }
  | { kind: 'image'; baseUrl: string | null; headUrl: string | null; base: SideInfo; head: SideInfo }
  | { kind: 'submodule' }

function toBase64(bytes: Uint8Array): string {
  let bin = ''
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  return btoa(bin)
}

const MAX_RASTER_SIDE = 4096

/**
 * Render SVG bytes to a PNG blob: URL. The SVG itself never gets a same-origin URL (it could run
 * script there if opened in a tab); it is decoded from an opaque-origin data: URL by <img>
 * semantics, which never run SVG script, and only the pixels come back (design.md §8).
 */
async function rasterizeSvg(buf: ArrayBuffer): Promise<string> {
  const img = new Image()
  img.src = `data:image/svg+xml;base64,${toBase64(new Uint8Array(buf))}`
  try {
    await img.decode()
  } catch {
    throw new Error('无法解析该 SVG 图片')
  }
  // SVGs without intrinsic size get the default replaced-element size, as <img> would.
  let w = img.naturalWidth || 300
  let h = img.naturalHeight || 150
  const scale = Math.min(1, MAX_RASTER_SIDE / Math.max(w, h))
  w = Math.max(1, Math.round(w * scale))
  h = Math.max(1, Math.round(h * scale))
  const canvas = document.createElement('canvas')
  canvas.width = w
  canvas.height = h
  canvas.getContext('2d')?.drawImage(img, 0, 0, w, h)
  const png = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, 'image/png'))
  if (!png) throw new Error('无法渲染该 SVG 图片')
  return URL.createObjectURL(png)
}

function imageUrl(buf: ArrayBuffer, type: ImageType): Promise<string> {
  if (type.kind === 'svg') return rasterizeSvg(buf)
  // Only types that can never become a document with script get a same-origin URL.
  if (!isSafeBlobMime(type.mime)) return Promise.reject(new Error(`不支持的图片类型 ${type.mime}`))
  return Promise.resolve(URL.createObjectURL(new Blob([buf], { type: type.mime })))
}

async function loadImage(repo: RepoRef, file: ChangedFile, type: ImageType): Promise<LoadedFile> {
  const one = async (oid: string | null) => {
    if (!oid) return { url: null, bytes: null }
    const buf = await (await getBlob(repo, oid)).arrayBuffer()
    // Bytes go through a blob: URL, never the API URL directly (design.md §8).
    return { url: await imageUrl(buf, type), bytes: buf.byteLength }
  }
  const settled = await Promise.allSettled([one(file.oldOid), one(file.newOid)])
  const failed = settled.find((r) => r.status === 'rejected')
  if (failed) {
    for (const r of settled) if (r.status === 'fulfilled' && r.value.url) URL.revokeObjectURL(r.value.url)
    throw failed.reason
  }
  const [b, h] = settled.map((r) => (r as PromiseFulfilledResult<{ url: string | null; bytes: number | null }>).value)
  return {
    kind: 'image',
    baseUrl: b.url,
    headUrl: h.url,
    base: { bytes: b.bytes },
    head: { bytes: h.bytes },
  }
}

async function loadText(repo: RepoRef, file: ChangedFile, force: boolean): Promise<LoadedFile> {
  const basePath = file.oldPath ?? file.path
  const sides: Array<{ side: Side; oid: string | null; path: string }> = [
    { side: 'base', oid: file.oldOid, path: basePath },
    { side: 'head', oid: file.newOid, path: file.path },
  ]

  // Models are pinned from the moment we hold them, so a concurrent load can't evict and
  // dispose them before the caller attaches them.
  const pinned: Monaco.editor.ITextModel[] = []
  const hold = (m: Monaco.editor.ITextModel) => {
    models.pin(m)
    pinned.push(m)
  }
  let released = false
  const release = () => {
    if (released) return
    released = true
    pinned.forEach((m) => models.unpin(m))
  }

  try {
    const resolved = await Promise.all(
      sides.map(async (s) => {
        const uri = modelUri(s.side, s.oid, s.path)
        const key = uri.toString()
        const cached = models.get(key)
        if (cached) {
          hold(cached)
          return { ...s, uri, key, cached, text: null as string | null, measured: { meta: modelMeta(cached) } as SideMeasure }
        }
        if (!s.oid) return { ...s, uri, key, cached: undefined, text: '', measured: { meta: measure('', 0) } as SideMeasure }
        const r = await fetchSideText(repo, s.oid, force)
        if (r.kind === 'tooLarge') return { ...s, uri, key, cached: undefined, text: null, measured: { tooLargeBytes: r.bytes } as SideMeasure }
        return { ...s, uri, key, cached: undefined, text: r.text, measured: { meta: r.meta } as SideMeasure }
      }),
    )

    const info = (i: number): SideInfo => {
      const r = resolved[i]
      if (!r.oid || !r.measured) return { bytes: null }
      return 'meta' in r.measured ? { bytes: r.measured.meta.bytes, lines: r.measured.meta.lines } : { bytes: r.measured.tooLargeBytes }
    }

    const decision = decideText(resolved.map((r) => (r.oid ? r.measured : null)), force)
    if (decision.kind === 'deferred') {
      release()
      return { kind: 'deferred', base: info(0), head: info(1) }
    }
    const { plainReason } = decision
    const language = plainReason ? 'plaintext' : languageForPath(file.path)

    const built = resolved.map((r) => {
      if (r.cached) {
        if (r.cached.getLanguageId() !== language) monaco.editor.setModelLanguage(r.cached, language)
        return r.cached
      }
      // A model with this URI can already exist if a concurrent load of the same file created it.
      const existing = monaco.editor.getModel(r.uri)
      const model = existing ?? monaco.editor.createModel(r.text ?? '', language, r.uri)
      if (r.measured && 'meta' in r.measured) metaOf.set(model, r.measured.meta)
      hold(model)
      models.put(r.key, model)
      return model
    })

    return { kind: 'text', original: built[0], modified: built[1], plainReason, forced: force, release }
  } catch (e) {
    release()
    throw e
  }
}

export async function loadFile(repo: RepoRef, file: ChangedFile, force: boolean): Promise<LoadedFile> {
  switch (fileDisplay(file)) {
    case 'submodule':
      // Gitlink oids are commits of another repository: nothing to fetch.
      return { kind: 'submodule' }
    case 'image':
      return loadImage(repo, file, imageType(file.path)!)
    case 'binary': {
      const [b, h] = await Promise.all([blobSize(repo, file.oldOid), blobSize(repo, file.newOid)])
      return { kind: 'binary', base: { bytes: b }, head: { bytes: h } }
    }
    case 'text':
      return loadText(repo, file, force)
  }
}

/** The first `n` bytes of a response body (fewer if it is shorter); the rest is not downloaded. */
async function readPrefix(res: Response, n: number): Promise<Uint8Array> {
  const reader = res.body?.getReader()
  if (!reader) return new Uint8Array(await res.arrayBuffer()).subarray(0, n)
  const out = new Uint8Array(n)
  let got = 0
  try {
    while (got < n) {
      const { done, value } = await reader.read()
      if (done) break
      const take = Math.min(value.length, n - got)
      out.set(value.subarray(0, take), got)
      got += take
    }
  } finally {
    await reader.cancel().catch(() => undefined)
  }
  return out.subarray(0, got)
}

/**
 * An unchanged file of the head tree, read-only. Git gives no binary verdict for these, so
 * the bytes are sniffed like git does; a large file is sniffed from its first bytes only and
 * then deferred (text) or shown by size (binary). Image extensions are read in full.
 */
export async function loadUnchanged(repo: RepoRef, entry: TreeEntry, force: boolean): Promise<LoadedFile> {
  if (isGitlink(entry.mode)) return { kind: 'submodule' }
  const none: SideInfo = { bytes: null }
  const uri = modelUri('head', entry.oid, entry.path)
  const key = uri.toString()

  let model = models.get(key)
  let meta: TextMeta
  let text = ''
  if (model) {
    meta = modelMeta(model)
  } else {
    const type = imageType(entry.path)
    const res = await getBlob(repo, entry.oid)
    const len = contentLength(res)
    if (!type && !force && len !== null && len > LIMITS.deferBytes) {
      const prefix = await readPrefix(res, SNIFF_BYTES)
      return { kind: looksBinary(prefix) ? 'binary' : 'deferred', base: none, head: { bytes: len } }
    }
    const buf = await res.arrayBuffer()
    if (looksBinary(new Uint8Array(buf))) {
      if (!type) return { kind: 'binary', base: none, head: { bytes: buf.byteLength } }
      // Bytes go through a blob: URL, never the API URL directly (design.md §8).
      const headUrl = await imageUrl(buf, type)
      return { kind: 'image', baseUrl: null, headUrl, base: none, head: { bytes: buf.byteLength } }
    }
    // Without a Content-Length the size is only known now: defer before decoding it all.
    if (!force && buf.byteLength > LIMITS.deferBytes) return { kind: 'deferred', base: none, head: { bytes: buf.byteLength } }
    text = decoder.decode(buf)
    meta = measure(text, buf.byteLength)
  }

  const decision = decideText([{ meta }], force)
  if (decision.kind === 'deferred') return { kind: 'deferred', base: none, head: { bytes: meta.bytes, lines: meta.lines } }
  const { plainReason } = decision
  const language = plainReason ? 'plaintext' : languageForPath(entry.path)
  if (model) {
    if (model.getLanguageId() !== language) monaco.editor.setModelLanguage(model, language)
  } else {
    // A model with this URI can already exist if a concurrent load of the same file created it.
    model = monaco.editor.getModel(uri) ?? monaco.editor.createModel(text, language, uri)
    metaOf.set(model, meta)
  }
  // Pinned before `put`, which may evict.
  models.pin(model)
  models.put(key, model)
  const held = model
  let released = false
  const release = () => {
    if (released) return
    released = true
    models.unpin(held)
  }
  return { kind: 'single', model, plainReason, forced: force, release }
}

/** Warm the model cache for a file the user is likely to open next. */
export function prefetchFile(repo: RepoRef, file: ChangedFile): void {
  if (fileDisplay(file) !== 'text') return
  loadText(repo, file, false).then(
    (r) => {
      if (r.kind === 'text') r.release()
    },
    () => undefined,
  )
}

export function formatBytes(n: number | null | undefined): string {
  if (n === null || n === undefined) return '—'
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  return `${(n / 1024 / 1024).toFixed(1)} MB`
}
