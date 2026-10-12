import { describe, expect, it } from 'vitest'
import type { ChangedFile } from './api'
import { LIMITS, PLAIN_REASONS, decideText, fileDisplay, imageType, isSafeBlobMime, measure, shortSha, type SideMeasure } from './degrade'

const meta = (bytes: number, lines = 10, maxLine = 80): SideMeasure => ({ meta: { bytes, lines, maxLine } })

describe('measure', () => {
  it('counts lines and the longest line', () => {
    expect(measure('a\nbcd\n', 6)).toEqual({ bytes: 6, lines: 3, maxLine: 3 })
    expect(measure('', 0)).toEqual({ bytes: 0, lines: 1, maxLine: 0 })
    expect(measure('x'.repeat(12), 12).maxLine).toBe(12)
  })
})

describe('decideText', () => {
  it('shows small files highlighted', () => {
    expect(decideText([meta(100), meta(200)], false)).toEqual({ kind: 'text', plainReason: null })
  })

  it('treats an absent side as empty', () => {
    expect(decideText([null, meta(100)], false)).toEqual({ kind: 'text', plainReason: null })
  })

  it('turns highlighting off above 1 MB', () => {
    expect(decideText([meta(10), meta(LIMITS.plainBytes + 1)], false)).toEqual({ kind: 'text', plainReason: PLAIN_REASONS.bytes })
  })

  it('turns highlighting off for very long lines', () => {
    expect(decideText([meta(10, 1, LIMITS.longLine + 1), meta(10)], false)).toEqual({ kind: 'text', plainReason: PLAIN_REASONS.longLine })
  })

  it('defers files over 5 MB, over 50k lines, or not downloaded because of their size', () => {
    expect(decideText([meta(LIMITS.deferBytes + 1), meta(10)], false)).toEqual({ kind: 'deferred' })
    expect(decideText([null, meta(1000, LIMITS.deferLines + 1)], false)).toEqual({ kind: 'deferred' })
    expect(decideText([{ tooLargeBytes: 6 << 20 }, meta(10)], false)).toEqual({ kind: 'deferred' })
  })

  it('shows a forced large file as plain text even when it is under 1 MB', () => {
    // 60k short lines: no other rule would turn highlighting off.
    expect(decideText([meta(500_000, 60_000, 8), meta(10)], true)).toEqual({ kind: 'text', plainReason: PLAIN_REASONS.forced })
  })

  it('does not change the decision for small files when forced', () => {
    expect(decideText([meta(10), meta(10)], true)).toEqual({ kind: 'text', plainReason: null })
  })
})

describe('image handling', () => {
  it('recognizes raster images by extension, case-insensitively', () => {
    expect(imageType('a/logo.PNG')).toEqual({ kind: 'raster', mime: 'image/png' })
    expect(imageType('x.jpeg')).toEqual({ kind: 'raster', mime: 'image/jpeg' })
    expect(imageType('fonts/Inter.woff2')).toBeNull()
    expect(imageType('Makefile')).toBeNull()
  })

  it('never classifies SVG as a raster type', () => {
    expect(imageType('assets/badge.svg')).toEqual({ kind: 'svg' })
    expect(imageType('assets/BADGE.SVG')).toEqual({ kind: 'svg' })
  })

  it('only allows raster types behind a same-origin blob: URL', () => {
    for (const ext of ['png', 'jpg', 'gif', 'webp', 'avif', 'bmp', 'ico']) {
      const t = imageType(`x.${ext}`)
      expect(t?.kind === 'raster' && isSafeBlobMime(t.mime)).toBe(true)
    }
    for (const mime of ['image/svg+xml', 'text/html', 'application/xhtml+xml', 'text/xml', 'application/xml', 'application/octet-stream', '']) {
      expect(isSafeBlobMime(mime)).toBe(false)
    }
  })
})

describe('fileDisplay', () => {
  const file = (extra: Partial<ChangedFile>) => ({ path: 'x', binary: false, oldMode: '100644', newMode: '100644', ...extra })

  it('shows gitlinks as submodules whatever else is set', () => {
    expect(fileDisplay(file({ path: 'vendor/lib', oldMode: '160000', newMode: '160000' }))).toBe('submodule')
    expect(fileDisplay(file({ path: 'vendor/lib', oldMode: '000000', newMode: '160000' }))).toBe('submodule')
    expect(fileDisplay(file({ path: 'vendor/lib.png', oldMode: '160000', newMode: '000000', binary: true }))).toBe('submodule')
  })

  it('shows binary images as images and other binaries as binary', () => {
    expect(fileDisplay(file({ path: 'a.png', binary: true }))).toBe('image')
    expect(fileDisplay(file({ path: 'a.svg', binary: true }))).toBe('image')
    expect(fileDisplay(file({ path: 'a.woff2', binary: true }))).toBe('binary')
  })

  it('diffs text SVGs as text', () => {
    expect(fileDisplay(file({ path: 'icons/check.svg' }))).toBe('text')
  })

  it('shortens shas to 7 characters', () => {
    expect(shortSha('3f8e1c2b7a9d4e6f0a1b2c3d4e5f60718293a4b5')).toBe('3f8e1c2')
  })
})
