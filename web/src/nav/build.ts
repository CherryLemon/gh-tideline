// Waiting for the index of a commit to be built: asking the server to build it, following the build, and what to do when it
// fails, vanishes or can't be followed. The calls, what is shown and the clock come in as arguments, so that every course the
// wait can take is tested (`build.test.ts`) with a fake clock. No React here.
import { ApiError, errorMessage, type IndexStatus } from '../api'

/** A look-up that takes shorter than this to get its index shows no dialog: one that is built already is no wait. */
export const GRACE_MS = 300
export const POLL_MS = 300
/** Times a look-up builds the index again: a job that vanished (the server restarted) or an index that went missing. */
export const MAX_BUILDS = 3
/** The waits before a call that failed is tried again; once they are used up, the failure is shown. */
export const RETRY_MS = [500, 1000, 2000]
/**
 * The longest a call that the server does not answer holds up the next: a stop waits for the starts of its build, and a start for the stops
 * of it, each for this long from when its call was sent. A stop that has to wait for a start that is not answered, and is not answered itself,
 * holds the next start up for twice this.
 */
export const STOP_WAIT_MS = 3000

export interface Clock {
  now(): number
  /** Resolves after `ms`, or at once when `signal` aborts. */
  sleep(ms: number, signal: AbortSignal): Promise<void>
}

export const sleep = (ms: number, signal: AbortSignal) =>
  new Promise<void>((resolve) => {
    if (signal.aborted) return resolve()
    const done = () => {
      clearTimeout(timer)
      signal.removeEventListener('abort', done)
      resolve()
    }
    const timer = setTimeout(done, ms)
    signal.addEventListener('abort', done, { once: true })
  })

export const realClock: Clock = { now: () => performance.now(), sleep }

/** What the wait asks of the server about the index of one commit. */
export interface BuildCalls {
  /** Starts the build, or joins the one under way; a commit that is indexed already answers `ready`. */
  start(): Promise<IndexStatus>
  poll(signal: AbortSignal): Promise<IndexStatus>
}

/** What the wait shows. */
export interface BuildView {
  /** The build is under way, and has been waited for long enough to show it. */
  progress(status: Pick<IndexStatus, 'phase' | 'done' | 'total'>): void
  /** The build failed, or can't be followed: shown with two ways on, `retry` to try again and `close` to give up. */
  failed(message: string, retry: () => void, close: () => void): void
  /** Something that became of the build that the user is to hear of. */
  notify(text: string): void
}

export interface Builds {
  /**
   * Asks the server, through `cancel`, to stop the build under `key`: once it has answered the starts of that build that are on their
   * way, or the stop would be overtaken by the start it is for. The next build under `key` starts once the server has answered the stop.
   * Neither is waited for longer than STOP_WAIT_MS: the stop has that long from when it is sent.
   */
  stop(key: string, cancel: () => Promise<unknown>): void
  /**
   * Waits for the index under `key` to be built, starting its build if need be. True once it is there; false when the wait is to
   * stop (aborted, the build was cancelled elsewhere, the user gave up); throws when the index can't be built or a call is refused.
   * An abort ends the wait at once, with a call that the server has not answered yet: that call goes on, and a stop waits for it.
   */
  wait(key: string, calls: BuildCalls, view: BuildView, signal: AbortSignal): Promise<boolean>
}

/**
 * Whether a failed call is worth another try: the server couldn't be reached or had a bad moment, rather than refusing the request
 * (an index it says it can't use is no better a moment later).
 */
function transient(e: unknown): boolean {
  if (e instanceof ApiError) return e.code !== 'index_unavailable' && (e.status === 0 || e.status >= 500 || e.status === 408 || e.status === 429)
  return !(e instanceof DOMException && e.name === 'AbortError')
}

export function createBuilds(clock: Clock = realClock): Builds {
  // The starts and the stops the server has not answered yet, by build. A start is in there for no longer than STOP_WAIT_MS, and a
  // stop for no longer than twice that (the start before it, then its own DELETE).
  const starting = new Map<string, Promise<void>>()
  const stopping = new Map<string, Promise<void>>()

  /** Waits for `call` to be answered, or to fail, but for no longer than STOP_WAIT_MS: one the server never answers holds nothing up for good. */
  const capped = async (call: Promise<unknown>): Promise<void> => {
    const timeout = new AbortController()
    try {
      await Promise.race([call.catch(() => {}), clock.sleep(STOP_WAIT_MS, timeout.signal)])
    } finally {
      timeout.abort()
    }
  }

  /** What `call` answers; null as soon as `signal` aborts, whatever the server is yet to say: the call itself is not cancelled. */
  const unlessAborted = <T>(signal: AbortSignal, call: Promise<T>): Promise<T | null> =>
    new Promise((resolve, reject) => {
      const abort = () => resolve(null)
      const done = () => signal.removeEventListener('abort', abort)
      // Handled even when the abort comes first, so that a call that fails after it is no unhandled rejection.
      call.then(
        (value) => {
          done()
          resolve(value)
        },
        (error) => {
          done()
          reject(error)
        },
      )
      if (signal.aborted) abort()
      else signal.addEventListener('abort', abort, { once: true })
    })

  /** Notes `call`, which is capped, as not answered yet under `key`, together with the calls noted before it, until it is. */
  const track = (pending: Map<string, Promise<void>>, key: string, call: Promise<void>) => {
    const answered = Promise.all([pending.get(key), call.catch(() => {})]).then(() => {})
    pending.set(key, answered)
    void answered.then(() => {
      if (pending.get(key) === answered) pending.delete(key)
    })
  }

  return {
    stop(key, cancel) {
      // Sent once the starts are answered, or past waiting for; from then on the stop has its own STOP_WAIT_MS.
      track(stopping, key, (starting.get(key) ?? Promise.resolve()).then(() => capped(cancel())))
    },

    async wait(key, calls, view, signal) {
      const started = clock.now()
      let shown = false
      // What the server says to a call about the build, or null once the wait is aborted: not waited for, whatever the call is at. A call
      // that fails for a moment (a gateway error, a dropped connection) is tried again; one that keeps failing is a failure of the build
      // as far as the user can tell.
      const ask = async (call: () => Promise<IndexStatus>): Promise<IndexStatus | null> => {
        for (let retries = 0; ; retries++) {
          try {
            return await unlessAborted(signal, call())
          } catch (e) {
            if (signal.aborted) return null
            if (!transient(e)) throw e
            if (retries >= RETRY_MS.length) return { commit: '', state: 'failed', phase: null, done: 0, total: 0, cancelling: false, message: errorMessage(e) }
            await clock.sleep(RETRY_MS[retries], signal)
            if (signal.aborted) return null
          }
        }
      }
      const check = () => ask(() => calls.poll(signal))
      // The server has heard of a stop before it hears of the build after it, or the stop (on its way still) would take that one down.
      const begin = async (): Promise<IndexStatus | null> => {
        await unlessAborted(signal, stopping.get(key) ?? Promise.resolve())
        if (signal.aborted) return null
        return ask(() => {
          const start = calls.start()
          track(starting, key, capped(start))
          return start
        })
      }
      // Whether the user wants to try again, once they have seen why not.
      const retry = (message: string) =>
        new Promise<boolean>((resolve) => {
          const end = (again: boolean) => {
            signal.removeEventListener('abort', close)
            resolve(again)
          }
          const close = () => end(false)
          signal.addEventListener('abort', close, { once: true })
          shown = true
          view.failed(message, () => end(true), close)
        })

      let status = await begin()
      for (let builds = 1; status && !signal.aborted; ) {
        switch (status.state) {
          case 'ready':
            return true
          case 'failed':
            // Until the user decides: the look-up is still theirs to go on with.
            if (!(await retry(status.message ?? 'Indexing failed'))) return false
            // The dialog goes back to the progress, and its Cancel, at once: not when the server has answered the start.
            view.progress({ phase: null, done: 0, total: 0 })
            status = await begin()
            continue
          case 'cancelled':
            // Not by this page, which would have stopped (another window did).
            view.notify('Indexing was cancelled')
            return false
          case 'missing':
            // The job is gone.
            if (++builds > MAX_BUILDS) throw new Error('The index could not be built')
            status = await begin()
            continue
          default:
            if (shown || clock.now() - started >= GRACE_MS) {
              shown = true
              view.progress(status)
            }
        }
        await clock.sleep(POLL_MS, signal)
        status = signal.aborted ? null : await check()
      }
      return false
    },
  }
}

/**
 * What `get` answers once the index of its commit is there. When `get` finds the commit not indexed, `build` waits for the index
 * (false: the look-up is to stop, so null), `built` is told, and `get` is asked again; the index is built again up to MAX_BUILDS times.
 */
export async function withIndex<T>(get: () => Promise<T>, build: () => Promise<boolean>, built: () => void): Promise<T | null> {
  for (let builds = 0; ; builds++) {
    try {
      return await get()
    } catch (e) {
      if (!(e instanceof ApiError && e.code === 'index_missing') || builds >= MAX_BUILDS) throw e
      if (!(await build())) return null
      built()
    }
  }
}
