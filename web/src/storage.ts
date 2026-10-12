import { useCallback, useState } from 'react'

function read<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key)
    return raw === null ? fallback : (JSON.parse(raw) as T)
  } catch {
    return fallback
  }
}

/** useState backed by localStorage (UI preferences only; failures fall back to in-memory). */
export function usePersistentState<T>(key: string, initial: T): [T, (v: T) => void] {
  const [value, setValue] = useState<T>(() => read(key, initial))
  const set = useCallback(
    (v: T) => {
      setValue(v)
      try {
        localStorage.setItem(key, JSON.stringify(v))
      } catch {
        // Storage unavailable (private mode, quota): keep the in-memory value.
      }
    },
    [key],
  )
  return [value, set]
}
