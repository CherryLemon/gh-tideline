import { describe, expect, it } from 'vitest'
import { DraftStore, INTERRUPTED, draftKey, type DraftStorage, type DraftTarget } from './drafts'

class FakeStorage implements DraftStorage {
  readonly items = new Map<string, string>()
  failWrites = false
  getItem(k: string) {
    return this.items.get(k) ?? null
  }
  setItem(k: string, v: string) {
    if (this.failWrites) throw new Error('QuotaExceededError')
    this.items.set(k, v)
  }
  removeItem(k: string) {
    this.items.delete(k)
  }
}

const HEAD = 'a'.repeat(40)
const FILE: DraftTarget = { kind: 'file', path: 'src/a:b.ts' }
const LINES: DraftTarget = { kind: 'lines', path: 'src/a.ts', side: 'RIGHT', startLine: 3, line: 5 }

function deferred() {
  let resolve!: () => void
  let reject!: (e: unknown) => void
  const promise = new Promise<void>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

describe('draftKey', () => {
  it('gives each target its own key, paths last', () => {
    expect(draftKey(FILE)).toBe('file:src/a:b.ts')
    expect(draftKey(LINES)).toBe('lines:RIGHT:3-5:src/a.ts')
    expect(draftKey({ ...LINES, startLine: null })).toBe('lines:RIGHT:-5:src/a.ts')
    expect(draftKey({ kind: 'reply', threadId: 'T1' })).toBe('reply:T1')
    expect(draftKey({ kind: 'edit', threadId: 'T1', commentId: 'C1' })).toBe('edit:C1')
    expect(draftKey({ kind: 'review' })).toBe('review')
  })
})

describe('DraftStore', () => {
  it('opens boxes, keeps their text across reloads and closes them', () => {
    const storage = new FakeStorage()
    const store = new DraftStore('github.com/o/r#1', storage)
    let changes = 0
    store.subscribe(() => changes++)
    const key = store.open(FILE, HEAD)
    expect(changes).toBe(1)
    expect(store.getSnapshot().get(key)).toEqual({ target: FILE, head: HEAD, sending: false, error: null })
    // Typing saves without a new snapshot.
    const before = store.getSnapshot()
    store.setText(key, 'Hello')
    expect(store.getSnapshot()).toBe(before)
    expect(changes).toBe(1)

    const again = new DraftStore('github.com/o/r#1', storage)
    expect(again.text(key)).toBe('Hello')
    expect(again.getSnapshot().get(key)?.target).toEqual(FILE)
    // Another pull request has its own.
    expect(new DraftStore('github.com/o/r#2', storage).getSnapshot().size).toBe(0)

    // Opening an open box keeps its text and head.
    again.open(FILE, 'b'.repeat(40))
    expect(again.text(key)).toBe('Hello')
    expect(again.getSnapshot().get(key)?.head).toBe(HEAD)

    again.close(key)
    expect(again.getSnapshot().size).toBe(0)
    expect(storage.items.size).toBe(0)
  })

  it('asks a box to take focus once per open', () => {
    const store = new DraftStore('s', null)
    const key = store.open(FILE, HEAD)
    expect(store.takeFocus('other')).toBe(false)
    expect(store.takeFocus(key)).toBe(true)
    expect(store.takeFocus(key)).toBe(false)
    store.open(FILE, HEAD)
    expect(store.takeFocus(key)).toBe(true)
  })

  it('closes a box once sent, keeps it with the error when sending fails', async () => {
    const store = new DraftStore('s', new FakeStorage())
    const key = store.open(LINES, HEAD)
    store.setText(key, 'Nit')
    const sent: string[] = []
    const first = deferred()
    const sending = store.send(key, (text) => {
      sent.push(text)
      return first.promise
    })
    expect(store.getSnapshot().get(key)?.sending).toBe(true)
    // A second send while the first is out does nothing.
    expect(await store.send(key, () => Promise.resolve())).toBe(false)
    first.reject(new Error('GitHub refused the change'))
    expect(await sending).toBe(false)
    expect(store.getSnapshot().get(key)).toMatchObject({ sending: false, error: 'GitHub refused the change' })
    expect(store.text(key)).toBe('Nit')

    expect(await store.send(key, async () => {})).toBe(true)
    expect(sent).toEqual(['Nit'])
    expect(store.getSnapshot().has(key)).toBe(false)
  })

  it('says a send cut short by a reload may have been posted', () => {
    const storage = new FakeStorage()
    const store = new DraftStore('s', storage)
    const key = store.open(FILE, HEAD)
    store.setText(key, 'Posting')
    void store.send(key, () => new Promise(() => {}))
    const reloaded = new DraftStore('s', storage)
    expect(reloaded.getSnapshot().get(key)).toMatchObject({ sending: false, error: INTERRUPTED })
    expect(reloaded.text(key)).toBe('Posting')
  })

  it('drops old, malformed and mismatched drafts', () => {
    const storage = new FakeStorage()
    const now = Date.UTC(2026, 9, 13)
    const day = 24 * 3600 * 1000
    const ok = { target: FILE, text: 'ok', head: HEAD, updatedAt: now - day }
    storage.setItem(
      'tl.drafts:s',
      JSON.stringify({
        v: 1,
        drafts: {
          [draftKey(FILE)]: ok,
          'file:old': { ...ok, target: { kind: 'file', path: 'old' }, updatedAt: now - 30 * day },
          'file:other': ok,
          'lines:RIGHT:-1:x': { ...ok, target: { kind: 'lines', path: 'x', side: 'UP', startLine: null, line: 1 } },
          review: { target: { kind: 'review' }, text: 5, head: HEAD, updatedAt: now },
        },
      }),
    )
    const store = new DraftStore('s', storage, now)
    expect([...store.getSnapshot().keys()]).toEqual([draftKey(FILE)])
    storage.setItem('tl.drafts:t', '{not json')
    expect(new DraftStore('t', storage, now).getSnapshot().size).toBe(0)
  })

  it('works without storage, or when writes fail', () => {
    const storage = new FakeStorage()
    storage.failWrites = true
    for (const s of [null, storage]) {
      const store = new DraftStore('s', s)
      const key = store.open(FILE, HEAD)
      store.setText(key, 'x')
      expect(store.text(key)).toBe('x')
    }
  })
})
