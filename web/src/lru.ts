// LRU cache whose values can be protected from eviction: values shown in the editor
// ("in use") and values a pending load has obtained but not yet attached ("pinned").

export class PinnedLru<V> {
  private readonly map = new Map<string, V>()
  private inUse = new Set<V>()
  private readonly pins = new Map<V, number>()

  constructor(
    private readonly capacity: number,
    /** Called on eviction. */
    private readonly dispose: (v: V) => void,
    /** False for values disposed elsewhere; they are dropped on lookup. */
    private readonly isAlive: (v: V) => boolean = () => true,
  ) {}

  get size(): number {
    return this.map.size
  }

  get(key: string): V | undefined {
    const hit = this.map.get(key)
    if (hit === undefined) return undefined
    this.map.delete(key)
    if (!this.isAlive(hit)) return undefined
    this.map.set(key, hit)
    return hit
  }

  put(key: string, value: V): void {
    this.map.delete(key)
    this.map.set(key, value)
    this.evict()
  }

  /** Values currently shown; replaces the previous set. */
  setInUse(values: V[]): void {
    this.inUse = new Set(values)
    this.evict()
  }

  /** Protect a value until the matching unpin (pins are counted). */
  pin(value: V): void {
    this.pins.set(value, (this.pins.get(value) ?? 0) + 1)
  }

  unpin(value: V): void {
    const n = this.pins.get(value) ?? 0
    if (n <= 1) this.pins.delete(value)
    else this.pins.set(value, n - 1)
    this.evict()
  }

  private evict(): void {
    if (this.map.size <= this.capacity) return
    for (const [key, value] of this.map) {
      if (this.map.size <= this.capacity) break
      if (this.inUse.has(value) || this.pins.has(value)) continue
      this.map.delete(key)
      this.dispose(value)
    }
  }
}
