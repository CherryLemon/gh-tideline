import { useEffect, useState } from 'react'
import { navigate, parsePrInput, parsePrPath, prPath, useLocation } from './router'
import { PrPage } from './components/PrPage'

export function App() {
  const { pathname, search, entryId } = useLocation()
  const route = parsePrPath(pathname)
  if (route) {
    const key = prPath(route)
    return <PrPage key={key} route={route} search={search} entryId={entryId} />
  }
  return <Home />
}

function Home() {
  const [value, setValue] = useState('')
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    document.title = 'Tideline'
  }, [])

  const submit = (e: React.FormEvent) => {
    e.preventDefault()
    const route = parsePrInput(value)
    if (!route) {
      setError('Not recognized. Paste a pull request link (https://github.com/owner/repo/pull/123) or owner/repo#123.')
      return
    }
    navigate(prPath(route))
  }

  return (
    <div className="home">
      <form className="home-card" onSubmit={submit}>
        <div className="home-title">Tideline</div>
        <div className="home-sub muted">Review GitHub pull requests in a JetBrains-style diff view</div>
        <input
          className="home-input"
          autoFocus
          spellCheck={false}
          placeholder="https://github.com/owner/repo/pull/123 or owner/repo#123"
          value={value}
          onChange={(e) => {
            setValue(e.target.value)
            setError(null)
          }}
        />
        {error && <div className="home-error">{error}</div>}
        <button className="btn btn-primary" type="submit">
          Open
        </button>
        <div className="home-tip muted">
          You can also open a GitHub link directly by replacing <code>https://</code> with <code>{location.origin}/</code>.
        </div>
      </form>
    </div>
  )
}
