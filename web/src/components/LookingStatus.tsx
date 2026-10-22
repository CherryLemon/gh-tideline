import { useEffect, useState } from 'react'
import { GRACE_MS } from '../nav/build'

/**
 * The status bar's word that a look-up is under way, once it has lasted a moment: a quick one is no news. The live region is there all
 * along and only its text comes and goes, which is the change a screen reader announces; one that is mounted with its text may not be.
 * `id` tells the look-up: one that replaces another under way has `looking` on throughout, and has the moment of its own all the same.
 */
export function LookingStatus({ looking, id }: { looking: boolean; id: number }) {
  // The look-up that has lasted a moment.
  const [late, setLate] = useState<number | null>(null)
  useEffect(() => {
    if (!looking) {
      setLate(null)
      return
    }
    const timer = setTimeout(() => setLate(id), GRACE_MS)
    return () => clearTimeout(timer)
  }, [looking, id])
  return (
    <span className="status-item status-looking" role="status">
      {looking && late === id ? 'Looking up…' : ''}
    </span>
  )
}
