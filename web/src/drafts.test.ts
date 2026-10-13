import { describe, expect, it } from 'vitest'
import { DraftStore, INTERRUPTED, draftKey, draftStoreFor, lostDrafts, type DraftStorage, type DraftTarget } from './drafts'
import { mkThread } from './test-fixtures'

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
    expect(draftKey({ kind: 'reply', threadId: 'T1', path: 'src/a.ts' })).toBe('reply:T1')
    expect(draftKey({ kind: 'edit', threadId: 'T1', commentId: 'C1', path: 'src/a.ts' })).toBe('edit:C1')
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

  it('opens an edit with the text of the comment, and keeps what was typed since', () => {
    const store = new DraftStore('s', new FakeStorage())
    const edit = { kind: 'edit', threadId: 'T1', commentId: 'C1', path: 'src/a.ts' } as const
    const key = store.open(edit, HEAD, 'As it was')
    expect(store.text(key)).toBe('As it was')
    store.setText(key, 'As it is')
    store.open(edit, HEAD, 'As it was')
    expect(store.text(key)).toBe('As it is')
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

  it('keeps a send that ends after its box was closed away from a box opened again', async () => {
    for (const outcome of ['posted', 'failed'] as const) {
      const storage = new FakeStorage()
      const store = new DraftStore('s', storage)
      const key = store.open(FILE, HEAD)
      store.setText(key, 'Old')
      const out = deferred()
      const sending = store.send(key, () => out.promise)
      store.close(key)
      expect(store.getSnapshot().has(key)).toBe(false)
      store.open(FILE, HEAD)
      expect(store.getSnapshot().get(key)).toEqual({ target: FILE, head: HEAD, sending: false, error: null })
      expect(store.text(key)).toBe('')
      store.setText(key, 'New')
      if (outcome === 'posted') out.resolve()
      else out.reject(new Error('nope'))
      expect(await sending).toBe(outcome === 'posted')
      expect(store.getSnapshot().get(key)).toEqual({ target: FILE, head: HEAD, sending: false, error: null })
      expect(store.text(key)).toBe('New')
      expect(new DraftStore('s', storage).text(key)).toBe('New')
    }
  })

  it('moves a draft to another head, not while it is sent', () => {
    const store = new DraftStore('s', new FakeStorage())
    const key = store.open(LINES, HEAD)
    const next = 'b'.repeat(40)
    const before = store.getSnapshot()
    store.setHead(key, next)
    expect(store.getSnapshot()).not.toBe(before)
    expect(store.getSnapshot().get(key)?.head).toBe(next)
    void store.send(key, () => new Promise(() => {}))
    store.setHead(key, HEAD)
    expect(store.getSnapshot().get(key)?.head).toBe(next)
  })

  it('has one store per scope for the page', () => {
    const storage = () => new FakeStorage()
    expect(draftStoreFor('x#1', storage)).toBe(draftStoreFor('x#1', storage))
    expect(draftStoreFor('x#2', storage)).not.toBe(draftStoreFor('x#1', storage))
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
          'reply:T1': { ...ok, target: { kind: 'reply', threadId: 'T1' } },
          review: { target: { kind: 'review' }, text: 5, head: HEAD, updatedAt: now },
        },
      }),
    )
    const store = new DraftStore('s', storage, now)
    expect([...store.getSnapshot().keys()]).toEqual([draftKey(FILE)])
    storage.setItem('tl.drafts:t', '{not json')
    expect(new DraftStore('t', storage, now).getSnapshot().size).toBe(0)
    storage.setItem('tl.drafts:u', JSON.stringify({ v: 2, drafts: { [draftKey(FILE)]: ok } }))
    expect(new DraftStore('u', storage, now).getSnapshot().size).toBe(0)
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

describe('lostDrafts', () => {
  it('lists replies and edits whose thread or comment is gone, not while they are sent', async () => {
    const store = new DraftStore('s', null)
    const kept = mkThread({ id: 'T1' })
    const reply = (threadId: string) => store.open({ kind: 'reply', threadId, path: 'src/a.ts' }, HEAD)
    const edit = (commentId: string) => store.open({ kind: 'edit', threadId: 'T1', commentId, path: 'src/a.ts' }, HEAD, 'x')
    store.open(FILE, HEAD)
    reply('T1')
    edit(kept.comments[0].id)
    const goneReply = reply('T9')
    const goneEdit = edit('C9')
    expect(lostDrafts(store.getSnapshot(), [kept])).toEqual([goneReply, goneEdit])
    const sending = store.send(goneReply, () => new Promise(() => {}))
    expect(lostDrafts(store.getSnapshot(), [kept])).toEqual([goneEdit])
    void sending
  })
})
