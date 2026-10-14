// Fake PR used by `pnpm dev:mock`. Covers every status (A/M/D/R/C/T), nested and
// compressible directories, generated files, a binary image, a non-image binary,
// a >1 MB file, a minified file, a >5 MB file, a submodule bump, a binary (UTF-16) SVG
// carrying a <script>, a text SVG and enough files to exercise the virtualized tree.
// The head tree adds unchanged files around them (full project tree), including an image,
// a non-image binary, a submodule and text and binary files over 5 MB.
import { createHash } from 'node:crypto'
import { crc32, deflateSync } from 'node:zlib'

export interface MockPr {
  host: string
  owner: string
  repo: string
  number: number
  title: string
  author: string
  state: 'OPEN' | 'CLOSED' | 'MERGED'
  url: string
  base: { ref: string; sha: string }
  head: { ref: string; sha: string }
  mergeBase: string
  nodeId: string
  viewer: string
}

export interface MockChangedFile {
  path: string
  oldPath?: string
  status: 'A' | 'M' | 'D' | 'R' | 'C' | 'T'
  oldOid: string | null
  newOid: string | null
  oldMode: string
  newMode: string
  additions?: number
  deletions?: number
  binary: boolean
  generated: boolean
  viewed?: 'VIEWED' | 'UNVIEWED' | 'DISMISSED'
}

interface Spec {
  path: string
  oldPath?: string
  status: MockChangedFile['status']
  old?: string | Buffer
  new?: string | Buffer
  oldMode?: string
  newMode?: string
  generated?: boolean
  binary?: boolean
  /** Submodule (gitlink, mode 160000): commit ids of the other repository, no blobs. */
  oldCommit?: string
  newCommit?: string
}

export const blobs = new Map<string, Buffer>()

function put(content: string | Buffer): string {
  const buf = typeof content === 'string' ? Buffer.from(content, 'utf8') : content
  const oid = createHash('sha1').update(`blob ${buf.length}\0`).update(buf).digest('hex')
  blobs.set(oid, buf)
  return oid
}

/** Approximate numstat: lines only in new / only in old (multiset difference). */
function numstat(a: string, b: string): { additions: number; deletions: number } {
  const count = (s: string) => {
    const m = new Map<string, number>()
    for (const l of s.split('\n')) m.set(l, (m.get(l) ?? 0) + 1)
    return m
  }
  const ca = count(a)
  const cb = count(b)
  let additions = 0
  let deletions = 0
  for (const [l, n] of cb) additions += Math.max(0, n - (ca.get(l) ?? 0))
  for (const [l, n] of ca) deletions += Math.max(0, n - (cb.get(l) ?? 0))
  return { additions, deletions }
}

// --- PNG generation (no dependencies) ---------------------------------------------

function chunk(type: string, data: Buffer): Buffer {
  const len = Buffer.alloc(4)
  len.writeUInt32BE(data.length)
  const td = Buffer.concat([Buffer.from(type, 'ascii'), data])
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(td) >>> 0)
  return Buffer.concat([len, td, crc])
}

function png(width: number, height: number, pixel: (x: number, y: number) => [number, number, number, number]): Buffer {
  const raw = Buffer.alloc((width * 4 + 1) * height)
  for (let y = 0; y < height; y++) {
    raw[y * (width * 4 + 1)] = 0
    for (let x = 0; x < width; x++) {
      const [r, g, b, a] = pixel(x, y)
      const o = y * (width * 4 + 1) + 1 + x * 4
      raw[o] = r
      raw[o + 1] = g
      raw[o + 2] = b
      raw[o + 3] = a
    }
  }
  const ihdr = Buffer.alloc(13)
  ihdr.writeUInt32BE(width, 0)
  ihdr.writeUInt32BE(height, 4)
  ihdr[8] = 8
  ihdr[9] = 6
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0)),
  ])
}

function logo(variant: 'old' | 'new'): Buffer {
  const size = variant === 'old' ? 160 : 192
  const c = size / 2
  return png(size, size, (x, y) => {
    const d = Math.hypot(x - c, y - c)
    if (variant === 'old') {
      if (d < c * 0.8) return [97, 130, 184, 255]
      return [0, 0, 0, 0]
    }
    if (d < c * 0.85 && d > c * 0.45) return [145, 184, 89, 255]
    if (d <= c * 0.45) return [246, 164, 52, 255]
    return [0, 0, 0, 0]
  })
}

// --- Text fixtures ------------------------------------------------------------------

const mainOld = `import { createServer, type IncomingMessage, type ServerResponse } from 'node:http'
import { readConfig } from './config'
import { log } from './util/log'

/** Application entry point. */
export interface AppOptions {
  port: number
  host: string
  verbose: boolean
}

const DEFAULT_PORT = 8080

${Array.from({ length: 30 }, (_, i) => `export function helper${i}(value: number): number {\n  // helper ${i} keeps the example long enough to fold unchanged regions\n  return value * ${i + 1} + ${i}\n}\n`).join('\n')}
function handle(req: IncomingMessage, res: ServerResponse): void {
  const url = new URL(req.url ?? '/', 'http://localhost')
  if (url.pathname === '/health') {
    res.writeHead(200, { 'content-type': 'text/plain' })
    res.end('ok')
    return
  }
  res.writeHead(404)
  res.end()
}

${Array.from({ length: 20 }, (_, i) => `export const CONSTANT_${i} = ${i * 7}\n`).join('')}
export function start(options: AppOptions): void {
  const server = createServer(handle)
  server.listen(options.port, options.host, () => {
    log('listening on ' + options.host + ':' + options.port)
  })
}

export function main(): void {
  const config = readConfig()
  start({ port: config.port ?? DEFAULT_PORT, host: '127.0.0.1', verbose: false })
}
`

const mainNew = mainOld
  .replace("import { log } from './util/log'", "import { log, warn } from './util/log'\nimport { slugify } from './util/strings'")
  .replace('  verbose: boolean\n}', '  verbose: boolean\n  /** Shut down after this many milliseconds of inactivity. */\n  idleTimeoutMs?: number\n}')
  .replace('const DEFAULT_PORT = 8080', 'const DEFAULT_PORT = 7457')
  .replace(
    "    res.end('ok')\n    return\n  }",
    "    res.end('ok')\n    return\n  }\n  if (url.pathname.startsWith('/p/')) {\n    const slug = slugify(url.pathname.slice(3))\n    res.writeHead(302, { location: '/posts/' + slug })\n    res.end()\n    return\n  }",
  )
  .replace("  res.writeHead(404)\n  res.end()", "  warn('not found: ' + url.pathname)\n  res.writeHead(404, { 'content-type': 'text/plain' })\n  res.end('not found')")
  .replace(
    "    log('listening on ' + options.host + ':' + options.port)\n  })",
    "    log(`listening on ${options.host}:${options.port}`)\n  })\n  if (options.idleTimeoutMs) server.setTimeout(options.idleTimeoutMs)",
  )
  .replace("host: '127.0.0.1', verbose: false", "host: config.host ?? '127.0.0.1', verbose: Boolean(config.verbose)")

const strings = `/** Convert a title into a URL slug. */
export function slugify(input: string): string {
  return input
    .normalize('NFKD')
    .replace(/[\\u0300-\\u036f]/g, '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
}

export function truncate(input: string, max = 80): string {
  return input.length <= max ? input : input.slice(0, max - 1) + '…'
}
`

const legacy = `// Deprecated API client, replaced by fetch-based client.
var request = require('request');

module.exports = function get(url, cb) {
  request({ url: url, json: true }, function (err, res, body) {
    if (err) return cb(err);
    cb(null, body);
  });
};
`

const btnOld = `import type { ReactNode } from 'react'

interface BtnProps {
  children: ReactNode
  onClick?: () => void
}

export function Btn({ children, onClick }: BtnProps) {
  return (
    <button className="btn" onClick={onClick}>
      {children}
    </button>
  )
}
`
const btnNew = btnOld
  .replace(/BtnProps/g, 'ButtonProps')
  .replace('export function Btn(', 'export function Button(')
  .replace('  onClick?: () => void\n}', "  onClick?: () => void\n  variant?: 'primary' | 'plain'\n}")
  .replace('{ children, onClick }', "{ children, onClick, variant = 'plain' }")
  .replace('className="btn"', 'className={`btn btn-${variant}`}')

const release = `#!/usr/bin/env bash
set -euo pipefail
version="$1"
git tag -a "v$version" -m "release $version"
git push origin "v$version"
`

const userRsOld = `use axum::{extract::Path, Json};
use serde::Serialize;

#[derive(Serialize)]
pub struct User {
    pub id: u64,
    pub name: String,
}

pub async fn get_user(Path(id): Path<u64>) -> Json<User> {
    Json(User { id, name: format!("user-{id}") })
}
`
const userRsNew = userRsOld
  .replace('    pub name: String,\n}', '    pub name: String,\n    pub email: Option<String>,\n}')
  .replace('Json(User { id, name: format!("user-{id}") })', 'let name = format!("user-{id}");\n    Json(User { id, name, email: None })')

const guideOld = `# Getting started

Install the CLI:

\`\`\`sh
cargo install tideline
\`\`\`

Then run \`gh tideline 123\` inside a repository.
`
const guideNew = guideOld.replace('cargo install tideline', 'cargo install --locked tideline').concat(
  '\n## Keyboard\n\n- **F7** next change\n- **Alt+↓** next file\n',
)

const readmeOld = `# demo-app

A small demo application.

## Development

Run \`pnpm dev\`.
`
const readmeNew = readmeOld.replace('A small demo application.', 'A small demo application used to exercise [tideline](https://github.com/acme/tideline).')

const cssOld = `:root {
  --brand: #6182b8;
  --radius: 4px;
}

.btn {
  border-radius: var(--radius);
  padding: 4px 8px;
}
`
const cssNew = cssOld.replace('--radius: 4px;', '--radius: 6px;\n  --accent: #53c7f0;').concat('\n.btn-primary {\n  background: var(--accent);\n}\n')

const goOld = `package main

import "fmt"

func main() {
\tfmt.Println("hello")
}
`
const goNew = goOld.replace('import "fmt"', 'import (\n\t"fmt"\n\t"os"\n)').replace('fmt.Println("hello")', 'fmt.Fprintln(os.Stderr, "hello", len(os.Args))')

const pyOld = `import json


def generate(path):
    with open(path) as f:
        data = json.load(f)
    return [item["name"] for item in data]
`
const pyNew = pyOld.replace('def generate(path):', 'def generate(path: str) -> list[str]:').replace('    return [item["name"] for item in data]', '    names = [item["name"] for item in data if item.get("enabled", True)]\n    return sorted(names)')

const yamlOld = `server:
  port: 8080
  host: 127.0.0.1
features:
  - search
`
const yamlNew = yamlOld.replace('port: 8080', 'port: 7457').concat('  - comments\n')

const configOld = `{
  "name": "demo",
  "port": 8080,
  "verbose": false
}
`
const configNew = configOld.replace('"port": 8080', '"port": 7457,\n  "host": "localhost"')

const htmlBase = `<!doctype html>
<html>
  <body>
    <h1 class="title">{{ title }}</h1>
    {{ content }}
  </body>
</html>
`
const htmlEmail = htmlBase.replace('<body>', '<body style="font-family: sans-serif">').replace('{{ content }}', '{{ content }}\n    <p class="footer">Sent by demo-app</p>')

function lockfile(version: string, count: number): string {
  let s = "lockfileVersion: '9.0'\n\npackages:\n"
  for (let i = 0; i < count; i++) {
    const v = i % 7 === 0 ? version : '1.0.0'
    s += `\n  pkg-${i}@${v}:\n    resolution: {integrity: sha512-${createHash('sha256').update(`${i}${v}`).digest('base64')}}\n`
  }
  return s
}

function cargoLock(version: string): string {
  let s = '# This file is automatically @generated by Cargo.\nversion = 4\n'
  for (let i = 0; i < 40; i++) s += `\n[[package]]\nname = "crate-${i}"\nversion = "${i === 3 ? version : '0.1.0'}"\n`
  return s
}

function bigCsv(seed: number): string {
  const rows = ['id,name,score,comment']
  for (let i = 0; i < 26000; i++) {
    const score = i === 1000 || i === 20000 ? i * seed : i * 3
    rows.push(`${i},row-${i},${score},"lorem ipsum dolor sit amet ${i % 97}"`)
  }
  return rows.join('\n') + '\n'
}

function minified(seed: number): string {
  const parts: string[] = []
  for (let i = 0; i < 1500; i++) parts.push(`function f${i}(a,b){return a*${i + seed}+b}`)
  return '!function(){"use strict";' + parts.join(';') + '}();\n'
}

function hugeJson(): string {
  const items: string[] = []
  for (let i = 0; i < 70000; i++) items.push(`  {"id": ${i}, "name": "item-${i}", "tags": ["a", "b", "c"], "value": ${i * 1.5}}`)
  return '[\n' + items.join(',\n') + '\n]\n'
}

function locale(lang: string, extra: boolean): string {
  const o: Record<string, string> = { hello: `Hello (${lang})`, bye: `Bye (${lang})`, save: `Save (${lang})` }
  if (extra) o.review = `Review (${lang})`
  return JSON.stringify(o, null, 2) + '\n'
}

const LOCALES = [
  'ar', 'bg', 'ca', 'cs', 'da', 'de', 'el', 'en', 'es', 'et', 'fa', 'fi', 'fr', 'he', 'hi', 'hr', 'hu',
  'id', 'it', 'ja', 'ko', 'lt', 'lv', 'nl', 'no', 'pl', 'pt', 'ro', 'ru', 'sk', 'sl', 'sv', 'th', 'tr',
  'uk', 'vi', 'zh-CN', 'zh-TW',
]

function fakeBinary(n: number, seed: number): Buffer {
  const b = Buffer.alloc(n)
  let x = seed
  for (let i = 0; i < n; i++) {
    x = (x * 1103515245 + 12345) & 0x7fffffff
    b[i] = x & 0xff
  }
  b.write('wOF2', 0, 'latin1')
  return b
}

function svg(fill: string): string {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">
  <rect x="4" y="4" width="56" height="56" rx="12" fill="${fill}"/>
  <path d="M18 33l9 9 19-20" fill="none" stroke="#fff" stroke-width="6" stroke-linecap="round" stroke-linejoin="round"/>
</svg>
`
}

/**
 * UTF-16 SVG (git reports it binary because of the NUL bytes) whose script would reveal
 * execution: it sets window.top.__pwned and requests /__pwned?origin=<its origin> from the dev server.
 */
function hostileSvg(fill: string): Buffer {
  const text = `<?xml version="1.0" encoding="UTF-16"?>
<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 96 96">
  <script>
    try { window.top.__pwned = true } catch (e) {}
    var o = location.origin === 'null' ? 'http://localhost:5173' : location.origin
    fetch(o + '/__pwned?origin=' + encodeURIComponent(location.origin), { mode: 'no-cors' })
  </script>
  <circle cx="48" cy="48" r="40" fill="${fill}"/>
</svg>
`
  return Buffer.from('\ufeff' + text, 'utf16le')
}

/**
 * A changed file whose first hunk is deep inside a method (`drain`): the diff editor hides the unchanged lines above it from line 1,
 * so a collapsed-region bar sits at the very top of the file, over code that is inside a class and a method.
 */
const workerOld = `// Jobs run one after another by a worker, each retried a few times.
import { log } from './app/util/log'

export interface Job {
  id: string
  attempts: number
  run(): Promise<void>
}

export class Worker {
  private readonly queue: Job[] = []
  private readonly failed: Job[] = []

  constructor(private readonly name: string) {
    log('worker ' + name + ' started')
  }

  push(job: Job): void {
    this.queue.push(job)
  }

  async drain(): Promise<number> {
    let done = 0
${Array.from({ length: 30 }, (_, i) => `    this.warm(${i})\n`).join('')}    while (this.queue.length > 0) {
      const job = this.queue.shift()!
      try {
        await job.run()
        done += 1
      } catch (err) {
        job.attempts += 1
        if (job.attempts < 3) {
          this.queue.push(job)
        } else {
          this.failed.push(job)
        }
      }
    }
    return done
  }

  private warm(step: number): void {
    log('warm ' + step)
  }

  report(): string {
    return this.name + ': ' + this.failed.length + ' failed'
  }
}
`
const workerNew = workerOld
  .replace(
    'if (job.attempts < 3) {\n          this.queue.push(job)',
    "if (job.attempts < 5) {\n          log('retry ' + job.id)\n          this.queue.push(job)",
  )
  .replace("' failed'\n", "' failed, ' + this.queue.length + ' waiting'\n")

const specs: Spec[] = [
  { path: 'src/app/main.ts', status: 'M', old: mainOld, new: mainNew },
  { path: 'src/app/util/strings.ts', status: 'A', new: strings },
  { path: 'src/app/config.json', status: 'M', old: configOld, new: configNew },
  { path: 'src/legacy/oldApi.js', status: 'D', old: legacy },
  { path: 'src/components/Button.tsx', oldPath: 'src/components/Btn.tsx', status: 'R', old: btnOld, new: btnNew },
  { path: 'src/styles/theme.css', status: 'M', old: cssOld, new: cssNew },
  { path: 'src/server/api/v1/handlers/user.rs', status: 'M', old: userRsOld, new: userRsNew },
  ...LOCALES.map((l, i): Spec => ({ path: `src/i18n/locales/${l}.json`, status: 'M', old: locale(l, false), new: locale(l, i % 3 !== 1) })),
  { path: 'scripts/release.sh', status: 'T', old: release, new: release, oldMode: '100644', newMode: '100755' },
  { path: 'docs/guide/getting-started.md', status: 'M', old: guideOld, new: guideNew },
  { path: 'server/main.go', status: 'M', old: goOld, new: goNew },
  { path: 'tools/gen.py', status: 'M', old: pyOld, new: pyNew },
  { path: 'config/settings.yaml', status: 'M', old: yamlOld, new: yamlNew },
  { path: 'templates/email.html', oldPath: 'templates/base.html', status: 'C', old: htmlBase, new: htmlEmail },
  { path: 'assets/logo.png', status: 'M', old: logo('old'), new: logo('new'), binary: true },
  { path: 'assets/badge.svg', status: 'M', old: hostileSvg('#6182b8'), new: hostileSvg('#f6a434'), binary: true },
  { path: 'assets/icons/check.svg', status: 'M', old: svg('#91b859'), new: svg('#39adb5') },
  {
    path: 'vendor/libgit2',
    status: 'M',
    oldCommit: '3f8e1c2b7a9d4e6f0a1b2c3d4e5f60718293a4b5',
    newCommit: '9c0d1e2f3a4b5c6d7e8f90a1b2c3d4e5f6a7b8c9',
    oldMode: '160000',
    newMode: '160000',
  },
  { path: 'assets/fonts/Inter.woff2', status: 'A', new: fakeBinary(48_213, 7), binary: true },
  { path: 'data/big.csv', status: 'M', old: bigCsv(3), new: bigCsv(5) },
  { path: 'public/vendor.bundle.js', status: 'M', old: minified(1), new: minified(2) },
  { path: 'data/fixtures/huge.json', status: 'A', new: hugeJson() },
  { path: 'README.md', status: 'M', old: readmeOld, new: readmeNew },
  { path: 'pnpm-lock.yaml', status: 'M', old: lockfile('2.0.0', 300), new: lockfile('2.1.0', 305), generated: true },
  { path: 'Cargo.lock', status: 'M', old: cargoLock('0.3.0'), new: cargoLock('0.4.0'), generated: true },
  { path: 'dist/app.min.js', status: 'A', new: minified(9), generated: true },
  { path: 'worker.ts', status: 'M', old: workerOld, new: workerNew },
]

function buildFiles(): MockChangedFile[] {
  return specs.map((s) => {
    const oldOid = s.oldCommit ?? (s.old !== undefined ? put(s.old) : null)
    const newOid = s.newCommit ?? (s.new !== undefined ? put(s.new) : null)
    const binary = s.binary ?? false
    const stat =
      binary
        ? {}
        : s.oldCommit !== undefined || s.newCommit !== undefined
          ? { additions: s.newCommit ? 1 : 0, deletions: s.oldCommit ? 1 : 0 } // what numstat reports for a gitlink
          : numstat(s.old === undefined ? '' : s.old.toString(), s.new === undefined ? '' : s.new.toString())
    const f: MockChangedFile = {
      path: s.path,
      status: s.status,
      oldOid,
      newOid,
      oldMode: oldOid === null ? '000000' : (s.oldMode ?? '100644'),
      newMode: newOid === null ? '000000' : (s.newMode ?? '100644'),
      binary,
      generated: s.generated ?? false,
      ...stat,
    }
    if (s.oldPath) f.oldPath = s.oldPath
    return f
  })
}

export const files: MockChangedFile[] = buildFiles()

/** Lines of a changed file on one side (LEFT = old, RIGHT = new), for review-thread fixtures. */
export function fileLines(path: string, side: 'LEFT' | 'RIGHT'): string[] {
  const f = files.find((f) => f.path === path)
  const oid = f && (side === 'LEFT' ? f.oldOid : f.newOid)
  const blob = oid ? blobs.get(oid) : undefined
  if (!blob) throw new Error(`mock: no ${side} text for ${path}`)
  return blob.toString('utf8').split('\n')
}

export interface MockTreeEntry {
  path: string
  mode: string
  oid: string
}

function unchangedText(path: string, i: number): string {
  return `// ${path}\n// Unchanged in this pull request.\n\nexport const id${i} = ${i}\n\nexport function value${i}(): number {\n  return id${i} * 2\n}\n`
}

function bigSql(): string {
  const rows: string[] = []
  for (let i = 0; i < 64_000; i++) rows.push(`INSERT INTO events (id, kind, payload) VALUES (${i}, 'kind-${i % 17}', '{"n": ${i}, "pad": "xxxxxxxxxxxxxxxx"}');`)
  return rows.join('\n') + '\n'
}

const registryChecks = Array.from({ length: 14 }, (_, i) => `  if (bands.low > ${i * 3}) {\n    lines.push('more than ${i * 3} low entries')\n  }\n`).join('')
const registryDefaults = Array.from({ length: 48 }, (_, i) => `  { id: 'entry-${i}', weight: ${(i * 7) % 100} },\n`).join('')

/** A source file with nested scopes (class, method, nested function), a long function and data after the last scope, for the sticky header. */
const registryTs = `// The registry of named entries, kept in weight order. Unchanged in this pull request.
import { log } from './log'

export interface Entry {
  id: string
  weight: number
  label?: string
}

export type Filter = (entry: Entry) => boolean

export const LIMITS = {
  maxEntries: 64,
  maxLabel: 40,
  minWeight: 0,
  maxWeight: 100,
  warnAt: 48,
}

export class Registry {
  private readonly entries = new Map<string, Entry>()

  constructor(private readonly name: string) {
    log('registry ' + name + ' opened')
  }

  get size(): number {
    return this.entries.size
  }

  add(entry: Entry): void {
    this.check(entry)
    this.entries.set(entry.id, entry)
    if (this.entries.size > LIMITS.warnAt) {
      log('registry ' + this.name + ' is almost full')
    }
  }

  find(id: string): Entry | undefined {
    return this.entries.get(id)
  }

  list(filter?: Filter): Entry[] {
    function byWeight(a: Entry, b: Entry): number {
      if (a.weight !== b.weight) {
        return b.weight - a.weight
      }
      // Equal weights are ordered by label.
      const left = a.label ?? a.id
      const right = b.label ?? b.id
      return left < right ? -1 : left > right ? 1 : 0
    }
    const all = Array.from(this.entries.values())
    const kept = filter ? all.filter(filter) : all
    return kept.sort(byWeight)
  }

  render(html = '<img src=x onerror=alert(1)>'): string {
    const items = this.list().map((entry) => '<li>' + (entry.label ?? entry.id) + '</li>')
    return html + '<ul>' + items.join('') + '</ul>'
  }

  private check(entry: Entry): void {
    if (entry.weight < LIMITS.minWeight || entry.weight > LIMITS.maxWeight) {
      throw new Error('weight out of range: ' + entry.weight)
    }
    if (entry.label && entry.label.length > LIMITS.maxLabel) {
      throw new Error('label too long: ' + entry.label)
    }
    if (this.entries.size >= LIMITS.maxEntries) {
      throw new Error('registry ' + this.name + ' is full')
    }
  }
}

/** One line per entry with its weight band, then a line per count that matters. */
export function summarize(registry: Registry): string[] {
  const lines: string[] = []
  const bands: Record<string, number> = { low: 0, mid: 0, high: 0 }
  for (const entry of registry.list()) {
    const band = entry.weight < 33 ? 'low' : entry.weight < 66 ? 'mid' : 'high'
    bands[band] += 1
    lines.push(band + ': ' + (entry.label ?? entry.id) + ' (' + entry.weight + ')')
  }
  lines.push('')
${registryChecks}  lines.push('total: ' + registry.size)
  return lines
}

export const DEFAULTS: Entry[] = [
${registryDefaults}]
`

/** A 25,000-line source file under the highlighting limit (1 MiB): what a jump far down a file costs the sticky header. */
function longSource(): string {
  const out: string[] = ['// A long file of small functions. Unchanged in this pull request.', '']
  for (let i = 0; out.length < 25_000; i++) {
    out.push(`export function field${i}(input: string, limit = ${i % 50}): string {`)
    out.push(`  const parts = input.split(',').map((part) => part.trim())`)
    out.push('  if (parts.length > limit) {')
    out.push(`    return parts.slice(0, limit).join(';') // keep ${i}`)
    out.push('  }')
    out.push("  return parts.join(';')")
    out.push('}')
    out.push('')
  }
  return out.join('\n') + '\n'
}

/** Files the PR doesn't touch: content (blob) or a commit (gitlink). */
function unchangedSpecs(): Array<{ path: string; content?: string | Buffer; commit?: string; mode?: string }> {
  const out: Array<{ path: string; content?: string | Buffer; commit?: string; mode?: string }> = []
  const text = (path: string) => out.push({ path, content: unchangedText(path, out.length) })
  for (let i = 0; i < 30; i++) text(`src/app/routes/route${i}.ts`)
  for (const name of ['Card', 'Modal', 'Tooltip', 'Tabs', 'Menu', 'Avatar', 'Badge', 'Spinner']) text(`src/components/${name}.tsx`)
  text('src/app/util/numbers.ts')
  for (let i = 0; i < 20; i++) text(`packages/core/src/deep/nested/mod${i}.ts`)
  for (let i = 0; i < 40; i++) text(`tests/unit/test_${i}.ts`)
  for (let i = 0; i < 20; i++) text(`docs/api/page${i}.md`)
  text('docs/guide/faq.md')
  text('server/handlers.go')
  text('tools/lint.py')
  for (const name of ['LICENSE', 'package.json', 'tsconfig.json', '.gitignore', 'Makefile']) text(name)
  out.push({ path: 'assets/screenshot.png', content: logo('old') })
  out.push({ path: 'assets/fonts/Mono.ttf', content: fakeBinary(23_456, 3) })
  out.push({ path: 'third_party/zlib', commit: '51b7f2abdade71cd9bb0e7a373ef2610ec6f9daf', mode: '160000' })
  out.push({ path: 'data/archive/events.sql', content: bigSql() })
  out.push({ path: 'data/archive/snapshot.bin', content: fakeBinary(5_400_000, 11) })
  out.push({ path: 'scripts/deploy.sh', content: '#!/bin/sh\nset -eu\necho deploy\n', mode: '100755' })
  out.push({ path: 'src/app/util/registry.ts', content: registryTs })
  out.push({ path: 'src/app/util/schema.ts', content: longSource() })
  return out
}

/** `git ls-tree -r` of the head: the changed files that still exist, plus the unchanged ones. */
function buildTree(): MockTreeEntry[] {
  const changed = files.filter((f) => f.newOid !== null).map((f) => ({ path: f.path, mode: f.newMode, oid: f.newOid! }))
  const unchanged = unchangedSpecs().map((u) => ({
    path: u.path,
    mode: u.mode ?? '100644',
    oid: u.commit ?? put(u.content!),
  }))
  return [...changed, ...unchanged].sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0))
}

export const tree: MockTreeEntry[] = buildTree()

/** Paths whose PUT /viewed fails: a GitHub error, and a head that moved meanwhile (409). */
export const VIEWED_FAILS = 'data/big.csv'
export const VIEWED_STALE = 'README.md'

/**
 * Viewed marks as GitHub would report them. The copied file has none, like a path GitHub's
 * rename detection does not list, so it gets no checkbox.
 */
export const viewed = new Map<string, NonNullable<MockChangedFile['viewed']>>(
  files.filter((f) => f.status !== 'C').map((f) => [f.path, 'UNVIEWED']),
)
viewed.set('src/app/config.json', 'VIEWED')
viewed.set('Cargo.lock', 'VIEWED')
viewed.set('src/styles/theme.css', 'DISMISSED')

export function filesWithViewed(): MockChangedFile[] {
  return files.map((f) => {
    const v = viewed.get(f.path)
    return v ? { ...f, viewed: v } : f
  })
}

const HEAD = 'b'.repeat(40)
let head = HEAD

/** Test hook: new commits on the pull request (`null`: back to the fixtures' head). */
export function setHead(sha: string | null): void {
  head = sha ?? HEAD
}

export function prSummary(host: string, owner: string, repo: string, n: number): MockPr {
  return {
    host,
    owner,
    repo,
    number: n,
    title: 'Add slug redirects, idle timeout and renamed Button component',
    // PR 43 is the viewer's own: GitHub refuses approving it or requesting changes on it.
    author: n === 43 ? 'mock-reviewer' : 'octocat',
    state: 'OPEN',
    url: `https://${host}/${owner}/${repo}/pull/${n}`,
    base: { ref: 'main', sha: 'a'.repeat(40) },
    head: { ref: 'feat/slug-redirects', sha: head },
    mergeBase: 'c'.repeat(40),
    nodeId: 'PR_kwDOMOCK',
    viewer: 'mock-reviewer',
  }
}
