// Waiting for the index of a commit to be built: asking the server to build it, following the build, and what to do when it
// fails, vanishes or can't be followed. The calls, what is shown and the clock come in as arguments, so that every course the
// wait can take is tested (`build.test.ts`) with a fake clock. No React here.
import { ApiError, errorMessage, type IndexStatus } from '../api'

/** A look-up that takes shorter than this to get its index shows no dialog: one that is built already is no wait. */
export const GRACE_MS = 300
export const POLL_MS = 300
/** Times a look-up builds the index again: a job that vanished (the server restarted) or an index that went missing. */
export const MAX_BUILDS = 3
/** The waits before a status check that failed is tried again; once they are used up, the failure is shown. */
export const RETRY_MS = [500, 1000, 2000]

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
  /** Asks the server, through `cancel`, to stop the build under `key`. The next build under `key` starts once the server has answered. */
  stop(key: string, cancel: () => Promise<unknown>): void
  /**
   * Waits for the index under `key` to be built, starting its build if need be. True once it is there; false when the wait is to
   * stop (aborted, the build was cancelled elsewhere, the user gave up); throws when the index can't be built or a call is refused.
   */
  wait(key: string, calls: BuildCalls, view: BuildView, signal: AbortSignal): Promise<boolean>
}

/** Whether a failed call is worth another try: the server couldn't be reached or had a bad moment, rather than refusing the request. */
function transient(e: unknown): boolean {
  if (e instanceof ApiError) return e.status === 0 || e.status >= 500 || e.status === 408 || e.status === 429
  return !(e instanceof DOMException && e.name === 'AbortError')
}

export function createBuilds(clock: Clock = realClock): Builds {
  // The stops the server has not answered yet.
  const stopping = new Map<string, Promise<void>>()

  return {
    stop(key, cancel) {
      const pending = Promise.all([stopping.get(key), cancel().catch(() => {})]).then(() => {})
      stopping.set(key, pending)
      void pending.then(() => {
        if (stopping.get(key) === pending) stopping.delete(key)
      })
    },

    async wait(key, calls, view, signal) {
      const started = clock.now()
      let shown = false
      // The server has heard of a stop before it hears of the build after it, or the stop (on its way still) would take that one down.
      const begin = async (): Promise<IndexStatus | null> => {
        await stopping.get(key)
        return signal.aborted ? null : calls.start()
      }
      // The status of the build. A check that fails for a moment (a gateway error, a dropped connection) is tried again; one that
      // keeps failing is a failure of the build as far as the user can tell.
      const check = async (): Promise<IndexStatus | null> => {
        for (let retries = 0; ; retries++) {
          try {
            return await calls.poll(signal)
          } catch (e) {
            if (signal.aborted) return null
            if (!transient(e)) throw e
            if (retries >= RETRY_MS.length) return { commit: '', state: 'failed', phase: null, done: 0, total: 0, cancelling: false, message: errorMessage(e) }
            await clock.sleep(RETRY_MS[retries], signal)
            if (signal.aborted) return null
          }
        }
      }
      // Whether the user wants to try again, once they have seen why not.
      const retry = (message: string) =>
        new Promise<boolean>((resolve) => {
          signal.addEventListener('abort', () => resolve(false), { once: true })
          shown = true
          view.failed(message, () => resolve(true), () => resolve(false))
        })

      let status = await begin()
      for (let builds = 1; status && !signal.aborted; ) {
        switch (status.state) {
          case 'ready':
            return true
          case 'failed':
            // Until the user decides: the look-up is still theirs to go on with.
            if (!(await retry(status.message ?? 'Indexing failed'))) return false
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
