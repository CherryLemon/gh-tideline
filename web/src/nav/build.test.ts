import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ApiError, type IndexStatus } from '../api'
import { GRACE_MS, MAX_BUILDS, POLL_MS, RETRY_MS, STOP_WAIT_MS, createBuilds, sleep, withIndex, type BuildCalls, type BuildView, type Clock } from './build'

const clock: Clock = { now: () => Date.now(), sleep }

beforeEach(() => {
  vi.useFakeTimers()
  vi.setSystemTime(0)
})
afterEach(() => {
  vi.useRealTimers()
})

const status = (state: IndexStatus['state'], extra: Partial<IndexStatus> = {}): IndexStatus => ({
  commit: 'c'.repeat(40),
  state,
  phase: null,
  done: 0,
  total: 0,
  cancelling: false,
  message: null,
  ...extra,
})

const apiError = (code: number, name = `http_${code}`) => new ApiError(code, name, '')

type Answer = IndexStatus | Error

/** A server that answers in turn, the last answer again and again when they run out; `log` notes each call and when it came. */
function server(starts: Answer[], polls: Answer[] = [status('ready')]) {
  const log: string[] = []
  const take = (answers: Answer[]) => {
    const answer = answers.length > 1 ? answers.shift()! : answers[0]
    if (answer instanceof Error) throw answer
    return answer
  }
  const calls: BuildCalls = {
    start: async () => {
      log.push(`start@${Date.now()}`)
      return take(starts)
    },
    poll: async () => {
      log.push(`poll@${Date.now()}`)
      return take(polls)
    },
  }
  return { calls, log }
}

/** What the user is shown, and the ways on offered with a failure. */
function screen() {
  const events: string[] = []
  const ways: Array<{ retry(): void; close(): void }> = []
  const view: BuildView = {
    progress: (s) => events.push(`progress ${s.phase ?? '-'} ${s.done}/${s.total}`),
    failed: (message, retry, close) => {
      events.push(`failed ${message}`)
      ways.push({ retry, close })
    },
    notify: (text) => events.push(`notify ${text}`),
  }
  return { view, events, ways }
}

/** How a promise stands, to be read after the clock has moved. */
function settle<T>(promise: Promise<T>) {
  const s: { state: 'pending' | 'resolved' | 'rejected'; value?: T; error?: unknown } = { state: 'pending' }
  promise.then(
    (value) => Object.assign(s, { state: 'resolved', value }),
    (error) => Object.assign(s, { state: 'rejected', error }),
  )
  return s
}

const tick = (ms = 0) => vi.advanceTimersByTimeAsync(ms)

/** A call the server never answers. */
const never = () => new Promise<never>(() => {})

/** A server whose starts wait for the test to answer them (`answer`, the oldest first); a check finds the index ready. */
function held() {
  const log: string[] = []
  const pending: Array<{ resolve(s: IndexStatus): void; reject(e: Error): void }> = []
  const calls: BuildCalls = {
    start: () =>
      new Promise((resolve, reject) => {
        log.push(`start@${Date.now()}`)
        pending.push({ resolve, reject })
      }),
    poll: async () => status('ready'),
  }
  return { calls, log, answer: (s: IndexStatus) => pending.shift()!.resolve(s), fail: (e: Error) => pending.shift()!.reject(e) }
}

describe('wait', () => {
  it('is over at once for an index that is built, and shows nothing', async () => {
    const srv = server([status('ready')])
    const scr = screen()
    const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
    await tick()
    expect(out).toMatchObject({ state: 'resolved', value: true })
    expect(srv.log).toEqual(['start@0'])
    expect(scr.events).toEqual([])
  })

  it('shows no dialog for a build that is done before the grace period is', async () => {
    expect(POLL_MS).toBeLessThanOrEqual(GRACE_MS)
    const srv = server([status('queued')], [status('ready')])
    const scr = screen()
    const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
    await tick(POLL_MS)
    expect(out).toMatchObject({ state: 'resolved', value: true })
    expect(srv.log).toEqual(['start@0', `poll@${POLL_MS}`])
    expect(scr.events).toEqual([])
  })

  it('shows the progress once the wait has lasted the grace period, and at every check after', async () => {
    const srv = server(
      [status('queued')],
      [status('building', { phase: 'fetching', done: 10, total: 40 }), status('building', { phase: 'parsing', done: 5, total: 40 }), status('ready')],
    )
    const scr = screen()
    const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
    await tick(GRACE_MS)
    expect(scr.events).toEqual(['progress fetching 10/40'])
    await tick(POLL_MS)
    expect(scr.events).toEqual(['progress fetching 10/40', 'progress parsing 5/40'])
    expect(out.state).toBe('pending')
    await tick(POLL_MS)
    expect(out).toMatchObject({ state: 'resolved', value: true })
  })

  it('checks on the build every POLL_MS', async () => {
    const abort = new AbortController()
    const srv = server([status('queued')], [status('building')])
    settle(createBuilds(clock).wait('k', srv.calls, screen().view, abort.signal))
    await tick(POLL_MS - 1)
    expect(srv.log).toEqual(['start@0'])
    await tick(1)
    expect(srv.log).toHaveLength(2)
    await tick(POLL_MS)
    expect(srv.log).toEqual(['start@0', `poll@${POLL_MS}`, `poll@${2 * POLL_MS}`])
    abort.abort()
  })

  describe('when the wait is aborted', () => {
    it('is over at once, between two checks', async () => {
      const abort = new AbortController()
      const srv = server([status('queued')])
      const out = settle(createBuilds(clock).wait('k', srv.calls, screen().view, abort.signal))
      await tick(100)
      abort.abort()
      await tick()
      expect(out).toMatchObject({ state: 'resolved', value: false })
      expect(srv.log).toEqual(['start@0'])
      expect(vi.getTimerCount()).toBe(0)
    })

    it('is over at once, in the middle of a check', async () => {
      const abort = new AbortController()
      const calls: BuildCalls = {
        start: async () => status('queued'),
        poll: (signal) => new Promise((_, reject) => signal.addEventListener('abort', () => reject(new DOMException('The operation was aborted.', 'AbortError')))),
      }
      const out = settle(createBuilds(clock).wait('k', calls, screen().view, abort.signal))
      await tick(POLL_MS)
      expect(out.state).toBe('pending')
      abort.abort()
      await tick()
      expect(out).toMatchObject({ state: 'resolved', value: false })
    })

    it('is over at once, while a failure is shown', async () => {
      const abort = new AbortController()
      const srv = server([status('failed', { message: 'disk full' })])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, abort.signal))
      await tick()
      expect(scr.events).toEqual(['failed disk full'])
      abort.abort()
      await tick()
      expect(out).toMatchObject({ state: 'resolved', value: false })
      expect(srv.log).toEqual(['start@0'])
    })

    it('is over at once, between the retries of a check', async () => {
      const abort = new AbortController()
      const srv = server([status('queued')], [apiError(502)])
      const out = settle(createBuilds(clock).wait('k', srv.calls, screen().view, abort.signal))
      await tick(POLL_MS + 100)
      expect(srv.log).toEqual(['start@0', `poll@${POLL_MS}`])
      abort.abort()
      await tick()
      expect(out).toMatchObject({ state: 'resolved', value: false })
      expect(vi.getTimerCount()).toBe(0)
    })

    it('is over at once, between the retries of a start', async () => {
      const abort = new AbortController()
      const srv = server([apiError(502)])
      const out = settle(createBuilds(clock).wait('k', srv.calls, screen().view, abort.signal))
      await tick(100)
      abort.abort()
      await tick()
      expect(out).toMatchObject({ state: 'resolved', value: false })
      expect(srv.log).toEqual(['start@0'])
      expect(vi.getTimerCount()).toBe(0)
    })

    it('shows nothing after the abort', async () => {
      const abort = new AbortController()
      const srv = server([status('queued')], [status('building')])
      const scr = screen()
      settle(createBuilds(clock).wait('k', srv.calls, scr.view, abort.signal))
      await tick(POLL_MS - 1)
      abort.abort()
      await tick(10 * POLL_MS)
      expect(scr.events).toEqual([])
      expect(srv.log).toEqual(['start@0'])
    })
  })

  describe('when the build fails', () => {
    it('shows why, and builds again when the user retries', async () => {
      const srv = server([status('failed', { message: 'Could not download the files' }), status('ready')])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      await tick(5 * POLL_MS)
      expect(scr.events).toEqual(['failed Could not download the files'])
      expect(out.state).toBe('pending')
      expect(srv.log).toEqual(['start@0'])
      scr.ways[0].retry()
      await tick()
      expect(out).toMatchObject({ state: 'resolved', value: true })
      expect(srv.log).toEqual(['start@0', `start@${5 * POLL_MS}`])
    })

    it('shows the progress at once after a retry: the dialog is up already, before the server has answered the start', async () => {
      const srv = held()
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      await tick()
      srv.answer(status('failed', { message: 'boom' }))
      await tick()
      expect(scr.events).toEqual(['failed boom'])
      scr.ways[0].retry()
      await tick()
      expect(srv.log).toHaveLength(2)
      expect(scr.events).toEqual(['failed boom', 'progress - 0/0'])
      srv.answer(status('queued'))
      await tick(POLL_MS)
      expect(scr.events).toEqual(['failed boom', 'progress - 0/0', 'progress - 0/0'])
      expect(out).toMatchObject({ state: 'resolved', value: true })
    })

    it('ends the wait when the dialog is cancelled right after a retry, with the start still on its way', async () => {
      const abort = new AbortController()
      const srv = held()
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, abort.signal))
      await tick()
      srv.answer(status('failed', { message: 'boom' }))
      await tick()
      scr.ways[0].retry()
      await tick()
      abort.abort()
      srv.answer(status('queued'))
      await tick(10 * POLL_MS)
      expect(out).toMatchObject({ state: 'resolved', value: false })
      expect(scr.events).toEqual(['failed boom', 'progress - 0/0'])
    })

    it('is over when the user closes it', async () => {
      const srv = server([status('failed', { message: 'boom' }), status('ready')])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      await tick()
      scr.ways[0].close()
      await tick()
      expect(out).toMatchObject({ state: 'resolved', value: false })
      expect(srv.log).toEqual(['start@0'])
    })

    it('asks again for as many retries as the user wants', async () => {
      const srv = server([status('failed', { message: 'one' }), status('failed', { message: 'two' }), status('ready')])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      await tick()
      scr.ways[0].retry()
      await tick()
      scr.ways[1].retry()
      await tick()
      expect(scr.events).toEqual(['failed one', 'progress - 0/0', 'failed two', 'progress - 0/0'])
      expect(out).toMatchObject({ state: 'resolved', value: true })
    })

    it('says so when the server gives no reason', async () => {
      const scr = screen()
      settle(createBuilds(clock).wait('k', server([status('failed')]).calls, scr.view, new AbortController().signal))
      await tick()
      expect(scr.events).toEqual(['failed Indexing failed'])
    })

    it('fails when the build fails further along, too', async () => {
      const srv = server([status('queued')], [status('building'), status('failed', { message: 'out of memory' })])
      const scr = screen()
      settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      await tick(2 * POLL_MS)
      expect(scr.events).toEqual(['progress - 0/0', 'failed out of memory'])
    })
  })

  describe('when the build was cancelled', () => {
    it('says so and stops: it was another window that did', async () => {
      const srv = server([status('queued')], [status('cancelled')])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      await tick(POLL_MS)
      expect(out).toMatchObject({ state: 'resolved', value: false })
      expect(scr.events).toEqual(['notify Indexing was cancelled'])
    })
  })

  describe('when the job is gone', () => {
    it('builds again', async () => {
      const srv = server([status('missing'), status('queued')], [status('ready')])
      const out = settle(createBuilds(clock).wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(POLL_MS)
      expect(out).toMatchObject({ state: 'resolved', value: true })
      expect(srv.log.filter((l) => l.startsWith('start'))).toHaveLength(2)
    })

    it('builds again when the job vanishes during the build', async () => {
      const srv = server([status('queued')], [status('building'), status('missing'), status('ready')])
      const out = settle(createBuilds(clock).wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(5 * POLL_MS)
      expect(out).toMatchObject({ state: 'resolved', value: true })
      expect(srv.log.filter((l) => l.startsWith('start'))).toHaveLength(2)
    })

    it(`gives up after ${MAX_BUILDS} builds`, async () => {
      const srv = server([status('missing')])
      const out = settle(createBuilds(clock).wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick()
      expect(out.state).toBe('rejected')
      expect(out.error).toEqual(new Error('The index could not be built'))
      expect(srv.log).toHaveLength(MAX_BUILDS)
    })

    it('counts the jobs that vanish during a build, too', async () => {
      const srv = server([status('queued')], [status('missing')])
      const out = settle(createBuilds(clock).wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(10 * POLL_MS)
      expect(out.state).toBe('rejected')
      expect(srv.log.filter((l) => l.startsWith('start'))).toHaveLength(MAX_BUILDS)
    })
  })

  describe('when a check fails', () => {
    it('tries it again after each of the waits of RETRY_MS, and goes on when it gets through', async () => {
      const srv = server([status('queued')], [apiError(502), apiError(502), apiError(502), status('ready')])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      const [first, second, third] = RETRY_MS
      await tick(POLL_MS + first - 1)
      expect(srv.log).toEqual(['start@0', `poll@${POLL_MS}`])
      await tick(1)
      expect(srv.log).toEqual(['start@0', `poll@${POLL_MS}`, `poll@${POLL_MS + first}`])
      await tick(second + third)
      expect(srv.log).toEqual([
        'start@0',
        `poll@${POLL_MS}`,
        `poll@${POLL_MS + first}`,
        `poll@${POLL_MS + first + second}`,
        `poll@${POLL_MS + first + second + third}`,
      ])
      expect(out).toMatchObject({ state: 'resolved', value: true })
      expect(scr.events).toEqual([])
    })

    it('shows the failure when it keeps failing, and builds again on Retry', async () => {
      const srv = server([status('queued'), status('ready')], [apiError(502)])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      await tick(POLL_MS + RETRY_MS[0] + RETRY_MS[1] + RETRY_MS[2])
      expect(srv.log.filter((l) => l.startsWith('poll'))).toHaveLength(RETRY_MS.length + 1)
      expect(scr.events).toEqual(['failed Request failed (HTTP 502)'])
      expect(out.state).toBe('pending')
      scr.ways[0].retry()
      await tick()
      expect(out).toMatchObject({ state: 'resolved', value: true })
      expect(srv.log.filter((l) => l.startsWith('start'))).toHaveLength(2)
    })

    it('is over when the user closes the failure', async () => {
      const srv = server([status('queued')], [apiError(0, 'network')])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      await tick(POLL_MS + RETRY_MS[0] + RETRY_MS[1] + RETRY_MS[2])
      expect(scr.events).toEqual(["failed Can't reach the Tideline server"])
      scr.ways[0].close()
      await tick()
      expect(out).toMatchObject({ state: 'resolved', value: false })
    })

    it.each([0, 408, 429, 500, 502, 503, 504])('tries again after a %i', async (code) => {
      const srv = server([status('queued')], [apiError(code, code === 0 ? 'network' : undefined), status('ready')])
      const out = settle(createBuilds(clock).wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(POLL_MS + RETRY_MS[0])
      expect(out).toMatchObject({ state: 'resolved', value: true })
    })

    it('tries again after a response that was cut short', async () => {
      const srv = server([status('queued')], [new TypeError('terminated'), status('ready')])
      const out = settle(createBuilds(clock).wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(POLL_MS + RETRY_MS[0])
      expect(out).toMatchObject({ state: 'resolved', value: true })
    })

    it.each([400, 401, 403, 404, 409])('does not try again after a %i: the request is refused', async (code) => {
      const refused = apiError(code, code === 409 ? 'stale_head' : undefined)
      const srv = server([status('queued')], [refused, status('ready')])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      await tick(POLL_MS + 10 * RETRY_MS[2])
      expect(out.state).toBe('rejected')
      expect(out.error).toBe(refused)
      expect(srv.log.filter((l) => l.startsWith('poll'))).toHaveLength(1)
      expect(scr.events).toEqual([])
    })

    it('does not try again when the server says it cannot use the index at all', async () => {
      const unusable = new ApiError(503, 'index_unavailable', 'the index database could not be opened')
      const srv = server([status('queued')], [unusable, status('ready')])
      const out = settle(createBuilds(clock).wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(POLL_MS + 10 * RETRY_MS[2])
      expect(out.state).toBe('rejected')
      expect(out.error).toBe(unusable)
      expect(srv.log.filter((l) => l.startsWith('poll'))).toHaveLength(1)
    })
  })

  describe('when the start fails', () => {
    it('tries it again after each of the waits of RETRY_MS, and goes on when it gets through', async () => {
      const srv = server([apiError(502), apiError(0, 'network'), apiError(503), status('ready')])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      const [first, second, third] = RETRY_MS
      await tick(first - 1)
      expect(srv.log).toEqual(['start@0'])
      await tick(1)
      expect(srv.log).toEqual(['start@0', `start@${first}`])
      await tick(second + third)
      expect(srv.log).toEqual(['start@0', `start@${first}`, `start@${first + second}`, `start@${first + second + third}`])
      expect(out).toMatchObject({ state: 'resolved', value: true })
      expect(scr.events).toEqual([])
    })

    it('tries again when the job is to be started anew, too', async () => {
      const srv = server([status('missing'), apiError(502), status('ready')])
      const out = settle(createBuilds(clock).wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(RETRY_MS[0])
      expect(out).toMatchObject({ state: 'resolved', value: true })
      expect(srv.log).toEqual(['start@0', 'start@0', `start@${RETRY_MS[0]}`])
    })

    it('shows the failure when it keeps failing, and starts again on Retry', async () => {
      const srv = server([apiError(502), apiError(502), apiError(502), apiError(502), status('ready')])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      await tick(RETRY_MS[0] + RETRY_MS[1] + RETRY_MS[2])
      expect(srv.log).toHaveLength(RETRY_MS.length + 1)
      expect(scr.events).toEqual(['failed Request failed (HTTP 502)'])
      expect(out.state).toBe('pending')
      scr.ways[0].retry()
      await tick()
      expect(out).toMatchObject({ state: 'resolved', value: true })
      expect(srv.log).toHaveLength(RETRY_MS.length + 2)
    })

    it('is over when the user closes the failure', async () => {
      const srv = server([apiError(0, 'network')])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      await tick(RETRY_MS[0] + RETRY_MS[1] + RETRY_MS[2])
      expect(scr.events).toEqual(["failed Can't reach the Tideline server"])
      scr.ways[0].close()
      await tick()
      expect(out).toMatchObject({ state: 'resolved', value: false })
    })

    it.each([400, 401, 403, 404, 409])('does not try again after a %i: the request is refused', async (code) => {
      const refused = apiError(code, code === 409 ? 'stale_head' : undefined)
      const srv = server([refused, status('ready')])
      const scr = screen()
      const out = settle(createBuilds(clock).wait('k', srv.calls, scr.view, new AbortController().signal))
      await tick(10 * RETRY_MS[2])
      expect(out.state).toBe('rejected')
      expect(out.error).toBe(refused)
      expect(srv.log).toEqual(['start@0'])
      expect(scr.events).toEqual([])
    })

    it('does not try again when the server says it cannot use the index at all', async () => {
      const unusable = new ApiError(503, 'index_unavailable', 'the index database could not be opened')
      const srv = server([unusable, status('ready')])
      const out = settle(createBuilds(clock).wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(10 * RETRY_MS[2])
      expect(out.state).toBe('rejected')
      expect(out.error).toBe(unusable)
      expect(srv.log).toEqual(['start@0'])
    })
  })

  describe('after a stop', () => {
    it('starts the build only once the server has heard of the stop', async () => {
      const builds = createBuilds(clock)
      const srv = server([status('ready')])
      let heard = false
      builds.stop(
        'k',
        () =>
          new Promise((resolve) =>
            setTimeout(() => {
              heard = true
              resolve(null)
            }, 200),
          ),
      )
      const out = settle(builds.wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(199)
      expect(srv.log).toEqual([])
      await tick(1)
      expect(heard).toBe(true)
      expect(srv.log).toEqual(['start@200'])
      expect(out).toMatchObject({ state: 'resolved', value: true })
    })

    it('does not wait for a stop that failed', async () => {
      const builds = createBuilds(clock)
      const srv = server([status('ready')])
      builds.stop('k', () => new Promise((_, reject) => setTimeout(() => reject(apiError(502)), 50)))
      const out = settle(builds.wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(50)
      expect(srv.log).toEqual(['start@50'])
      expect(out).toMatchObject({ state: 'resolved', value: true })
    })

    it('does not wait for a stop of another build', async () => {
      const builds = createBuilds(clock)
      const srv = server([status('ready')])
      builds.stop('other', () => new Promise((resolve) => setTimeout(resolve, 200)))
      settle(builds.wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick()
      expect(srv.log).toEqual(['start@0'])
    })

    it('does not wait for a stop the server has answered', async () => {
      const builds = createBuilds(clock)
      builds.stop('k', () => Promise.resolve(null))
      await tick()
      const srv = server([status('ready')])
      settle(builds.wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick()
      expect(srv.log).toEqual(['start@0'])
    })

    it('waits for every stop sent before it', async () => {
      const builds = createBuilds(clock)
      const srv = server([status('ready')])
      builds.stop('k', () => new Promise((resolve) => setTimeout(resolve, 300)))
      builds.stop('k', () => Promise.resolve(null))
      settle(builds.wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(299)
      expect(srv.log).toEqual([])
      await tick(1)
      expect(srv.log).toEqual(['start@300'])
    })

    it('starts nothing for a wait that was aborted while it waited, and is over with it at once', async () => {
      const builds = createBuilds(clock)
      const abort = new AbortController()
      const srv = server([status('ready')])
      builds.stop('k', () => new Promise((resolve) => setTimeout(resolve, 200)))
      const out = settle(builds.wait('k', srv.calls, screen().view, abort.signal))
      await tick(100)
      abort.abort()
      await tick()
      expect(out).toMatchObject({ state: 'resolved', value: false })
      await tick(200)
      expect(srv.log).toEqual([])
    })

    describe('that the server never answers', () => {
      it(`is waited for no longer than ${STOP_WAIT_MS} ms`, async () => {
        const builds = createBuilds(clock)
        const srv = server([status('ready')])
        builds.stop('k', never)
        const out = settle(builds.wait('k', srv.calls, screen().view, new AbortController().signal))
        await tick(STOP_WAIT_MS - 1)
        expect(srv.log).toEqual([])
        expect(out.state).toBe('pending')
        await tick(1)
        expect(srv.log).toEqual([`start@${STOP_WAIT_MS}`])
        expect(out).toMatchObject({ state: 'resolved', value: true })
        expect(vi.getTimerCount()).toBe(0)
      })

      it('holds up no build that comes after the wait is over', async () => {
        const builds = createBuilds(clock)
        builds.stop('k', never)
        await tick(STOP_WAIT_MS)
        const srv = server([status('ready')])
        const out = settle(builds.wait('k', srv.calls, screen().view, new AbortController().signal))
        await tick()
        expect(srv.log).toEqual([`start@${STOP_WAIT_MS}`])
        expect(out).toMatchObject({ state: 'resolved', value: true })
      })

      it('does not make a stop that comes after it wait any longer', async () => {
        const builds = createBuilds(clock)
        const srv = server([status('ready')])
        builds.stop('k', never)
        await tick(1000)
        builds.stop('k', () => Promise.resolve(null))
        settle(builds.wait('k', srv.calls, screen().view, new AbortController().signal))
        await tick(STOP_WAIT_MS - 1000 - 1)
        expect(srv.log).toEqual([])
        await tick(1)
        expect(srv.log).toEqual([`start@${STOP_WAIT_MS}`])
      })

      it('is not waited for past an abort: the wait is over at once', async () => {
        const builds = createBuilds(clock)
        const abort = new AbortController()
        const srv = server([status('ready')])
        builds.stop('k', never)
        const out = settle(builds.wait('k', srv.calls, screen().view, abort.signal))
        await tick(100)
        expect(out.state).toBe('pending')
        abort.abort()
        await tick()
        expect(out).toMatchObject({ state: 'resolved', value: false })
        expect(srv.log).toEqual([])
        // What is left is the stop's own limit.
        expect(vi.getTimerCount()).toBe(1)
        await tick(STOP_WAIT_MS)
        expect(vi.getTimerCount()).toBe(0)
        expect(srv.log).toEqual([])
      })

      it('is not waited for by a wait that was aborted before it began', async () => {
        const builds = createBuilds(clock)
        const abort = new AbortController()
        abort.abort()
        const srv = server([status('ready')])
        builds.stop('k', never)
        const out = settle(builds.wait('k', srv.calls, screen().view, abort.signal))
        await tick()
        expect(out).toMatchObject({ state: 'resolved', value: false })
        expect(srv.log).toEqual([])
      })
    })
  })

  describe('after a start', () => {
    /** A stop that notes when it is sent, in the log of the server. */
    const stopNoting = (log: string[]) => async () => {
      log.push(`stop@${Date.now()}`)
      return null
    }

    it('sends the stop only once the server has answered the start that is on its way', async () => {
      const builds = createBuilds(clock)
      const abort = new AbortController()
      const srv = held()
      settle(builds.wait('k', srv.calls, screen().view, abort.signal))
      await tick(50)
      abort.abort()
      builds.stop('k', stopNoting(srv.log))
      await tick(100)
      expect(srv.log).toEqual(['start@0'])
      srv.answer(status('queued'))
      await tick()
      expect(srv.log).toEqual(['start@0', 'stop@150'])
    })

    it('sends it after a start that failed, too', async () => {
      const builds = createBuilds(clock)
      const abort = new AbortController()
      const srv = held()
      settle(builds.wait('k', srv.calls, screen().view, abort.signal))
      await tick(50)
      abort.abort()
      builds.stop('k', stopNoting(srv.log))
      srv.fail(apiError(404))
      await tick()
      expect(srv.log).toEqual(['start@0', 'stop@50'])
    })

    it('sends it at once when the server has answered every start', async () => {
      const builds = createBuilds(clock)
      const log: string[] = []
      const srv = server([status('ready')])
      settle(builds.wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(50)
      builds.stop('k', stopNoting(log))
      await tick()
      expect(log).toEqual(['stop@50'])
    })

    it('does not wait for the start of another build', async () => {
      const builds = createBuilds(clock)
      const srv = held()
      const log: string[] = []
      settle(builds.wait('other', srv.calls, screen().view, new AbortController().signal))
      await tick(50)
      builds.stop('k', stopNoting(log))
      await tick()
      expect(log).toEqual(['stop@50'])
    })

    it(`sends it without the answer to a start that never comes, ${STOP_WAIT_MS} ms after that start`, async () => {
      const builds = createBuilds(clock)
      const log: string[] = []
      const calls: BuildCalls = { start: never, poll: async () => status('ready') }
      settle(builds.wait('k', calls, screen().view, new AbortController().signal))
      await tick(50)
      builds.stop('k', stopNoting(log))
      await tick(STOP_WAIT_MS - 51)
      expect(log).toEqual([])
      await tick(1)
      expect(log).toEqual([`stop@${STOP_WAIT_MS}`])
    })

    it('keeps the next build waiting for the stop, and so for the start before it', async () => {
      const builds = createBuilds(clock)
      const abort = new AbortController()
      const srv = held()
      settle(builds.wait('k', srv.calls, screen().view, abort.signal))
      await tick(50)
      abort.abort()
      builds.stop('k', stopNoting(srv.log))
      settle(builds.wait('k', srv.calls, screen().view, new AbortController().signal))
      await tick(100)
      expect(srv.log).toEqual(['start@0'])
      srv.answer(status('queued'))
      await tick()
      expect(srv.log).toEqual(['start@0', 'stop@150', 'start@150'])
    })
  })
})

describe('withIndex', () => {
  const missing = () => new ApiError(409, 'index_missing', 'the commit has not been indexed')

  it('answers what the first ask answers, without a build', async () => {
    const build = vi.fn(async () => true)
    await expect(withIndex(async () => 'found', build, () => {})).resolves.toBe('found')
    expect(build).not.toHaveBeenCalled()
  })

  it('builds the index when it is missing, and asks again', async () => {
    const log: string[] = []
    let built = false
    const answer = await withIndex(
      async () => {
        log.push('get')
        if (!built) throw missing()
        return 'found'
      },
      async () => {
        log.push('build')
        built = true
        return true
      },
      () => log.push('built'),
    )
    expect(answer).toBe('found')
    expect(log).toEqual(['get', 'build', 'built', 'get'])
  })

  it('is null, asking no more, when the build says the look-up is to stop', async () => {
    const get = vi.fn(async () => {
      throw missing()
    })
    const built = vi.fn()
    await expect(withIndex(get, async () => false, built)).resolves.toBeNull()
    expect(get).toHaveBeenCalledTimes(1)
    expect(built).not.toHaveBeenCalled()
  })

  it(`builds up to ${MAX_BUILDS} times for an index that is built and yet missing, then fails with that`, async () => {
    const error = missing()
    const get = vi.fn(async () => {
      throw error
    })
    const build = vi.fn(async () => true)
    await expect(withIndex(get, build, () => {})).rejects.toBe(error)
    expect(build).toHaveBeenCalledTimes(MAX_BUILDS)
    expect(get).toHaveBeenCalledTimes(MAX_BUILDS + 1)
  })

  it('fails at once with any other error', async () => {
    for (const error of [new ApiError(409, 'stale_head', ''), new ApiError(503, 'index_unavailable', ''), new Error('boom')]) {
      const build = vi.fn(async () => true)
      await expect(withIndex(() => Promise.reject(error), build, () => {})).rejects.toBe(error)
      expect(build).not.toHaveBeenCalled()
    }
  })
})
