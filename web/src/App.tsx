import { useEffect, useState } from 'react'
import { navigate, parsePrInput, parsePrPath, prPath, useLocation } from './router'
import { PrPage } from './components/PrPage'

export function App() {
  const { pathname, search } = useLocation()
  const route = parsePrPath(pathname)
  if (route) {
    const key = prPath(route)
    return <PrPage key={key} route={route} search={search} />
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
      setError('无法识别。请粘贴 PR 链接（https://github.com/owner/repo/pull/123）或 owner/repo#123')
      return
    }
    navigate(prPath(route))
  }

  return (
    <div className="home">
      <form className="home-card" onSubmit={submit}>
        <div className="home-title">Tideline</div>
        <div className="home-sub muted">在 JetBrains 风格的 diff 视图里 review GitHub PR</div>
        <input
          className="home-input"
          autoFocus
          spellCheck={false}
          placeholder="https://github.com/owner/repo/pull/123 或 owner/repo#123"
          value={value}
          onChange={(e) => {
            setValue(e.target.value)
            setError(null)
          }}
        />
        {error && <div className="home-error">{error}</div>}
        <button className="btn btn-primary" type="submit">
          打开
        </button>
        <div className="home-tip muted">
          也可以把 GitHub 链接里的 <code>https://</code> 换成 <code>{location.origin}/</code> 直接打开。
        </div>
      </form>
    </div>
  )
}
