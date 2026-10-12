import { describe, expect, it } from 'vitest'
import { PinnedLru } from './lru'

interface Fake {
  id: string
  disposed: boolean
}

function setup(capacity: number) {
  const disposed: string[] = []
  const lru = new PinnedLru<Fake>(
    capacity,
    (v) => {
      v.disposed = true
      disposed.push(v.id)
    },
    (v) => !v.disposed,
  )
  const make = (id: string): Fake => ({ id, disposed: false })
  return { lru, disposed, make }
}

describe('PinnedLru', () => {
  it('evicts the least recently used value beyond capacity', () => {
    const { lru, disposed, make } = setup(2)
    lru.put('a', make('a'))
    lru.put('b', make('b'))
    lru.get('a')
    lru.put('c', make('c'))
    expect(disposed).toEqual(['b'])
    expect(lru.get('b')).toBeUndefined()
    expect(lru.get('a')?.id).toBe('a')
  })

  it('never evicts values in use', () => {
    const { lru, disposed, make } = setup(1)
    const a = make('a')
    lru.put('a', a)
    lru.setInUse([a])
    lru.put('b', make('b'))
    expect(disposed).toEqual(['b'])
    expect(lru.get('a')).toBe(a)
  })

  it('protects a pinned value obtained by a pending load until it is unpinned', () => {
    const { lru, disposed, make } = setup(1)
    const a = make('a')
    lru.put('a', a)
    // A load takes `a` from the cache, then waits for the other side...
    const held = lru.get('a')!
    lru.pin(held)
    // ...while another load fills the cache.
    lru.put('b', make('b'))
    lru.put('c', make('c'))
    expect(a.disposed).toBe(false)
    expect(disposed).toEqual(['b', 'c'])
    // Attached: in use, then released.
    lru.setInUse([held])
    lru.unpin(held)
    expect(a.disposed).toBe(false)
    // Detached: now it is an ordinary entry and the next insertion evicts it.
    lru.setInUse([])
    expect(a.disposed).toBe(false)
    lru.put('d', make('d'))
    expect(a.disposed).toBe(true)
    expect(lru.size).toBe(1)
  })

  it('counts pins from concurrent loads of the same value', () => {
    const { lru, make } = setup(1)
    const a = make('a')
    lru.put('a', a)
    lru.pin(a)
    lru.pin(a)
    lru.put('b', make('b'))
    lru.unpin(a)
    lru.put('c', make('c'))
    expect(a.disposed).toBe(false)
    lru.unpin(a)
    lru.put('d', make('d'))
    expect(a.disposed).toBe(true)
  })

  it('evicts an abandoned load once its pins are released', () => {
    const { lru, make } = setup(1)
    const a = make('a')
    lru.pin(a)
    lru.put('a', a)
    lru.put('b', make('b'))
    expect(a.disposed).toBe(false)
    lru.unpin(a)
    expect(a.disposed).toBe(false)
    lru.put('c', make('c'))
    expect(a.disposed).toBe(true)
  })

  it('drops values that were disposed elsewhere', () => {
    const { lru, make } = setup(4)
    const a = make('a')
    lru.put('a', a)
    a.disposed = true
    expect(lru.get('a')).toBeUndefined()
    expect(lru.size).toBe(0)
  })
})
