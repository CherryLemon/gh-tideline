import { useEffect, useState } from 'react'
import { GRACE_MS } from '../nav/build'

/**
 * The status bar's word that a look-up is under way, once it has lasted a moment: a quick one is no news. The live region is there all
 * along and only its text comes and goes, which is the change a screen reader announces; one that is mounted with its text may not be.
 */
export function LookingStatus({ looking }: { looking: boolean }) {
  const [late, setLate] = useState(false)
  useEffect(() => {
    if (!looking) {
      setLate(false)
      return
    }
    const timer = setTimeout(() => setLate(true), GRACE_MS)
    return () => clearTimeout(timer)
  }, [looking])
  return (
    <span className="status-item status-looking" role="status">
      {looking && late ? 'Looking up…' : ''}
    </span>
  )
}
