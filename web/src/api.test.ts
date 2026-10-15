import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError, CANCEL_INDEX_TIMEOUT_MS, START_INDEX_TIMEOUT_MS, cancelIndex, startIndex, type RepoRef } from './api'

const repo: RepoRef = { host: 'github.com', owner: 'octo', repo: 'hello' }

afterEach(() => {
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

/** `AbortSignal.timeout` with the clock in the hands of the test. */
function timeouts() {
  const controllers: AbortController[] = []
  const lasts: number[] = []
  vi.spyOn(AbortSignal, 'timeout').mockImplementation((ms) => {
    const controller = new AbortController()
    controllers.push(controller)
    lasts.push(ms)
    return controller.signal
  })
  return {
    /** How long each signal given was to last. */
    lasts,
    /** The time is up for the `n`th signal given, as the browser has it. */
    pass(n: number) {
      controllers[n].abort(new DOMException('The operation timed out.', 'TimeoutError'))
    },
  }
}

/** A server that never answers: a request ends only with the reason of its signal, as with `fetch`. */
function silent() {
  const sent: Array<{ path: string; method: string | undefined }> = []
  vi.stubGlobal(
    'fetch',
    (path: string, init: RequestInit) =>
      new Promise((_, reject) => {
        sent.push({ path, method: init.method })
        init.signal?.addEventListener('abort', () => reject(init.signal?.reason))
      }),
  )
  return sent
}

describe('the start of an index build', () => {
  it(`is sent with a limit of ${START_INDEX_TIMEOUT_MS} ms`, () => {
    const clock = timeouts()
    const sent = silent()
    void startIndex(repo, 7, 'abc', 'head').catch(() => {})
    expect(sent).toEqual([{ path: '/api/github.com/octo/hello/pulls/7/index', method: 'POST' }])
    expect(clock.lasts).toEqual([START_INDEX_TIMEOUT_MS])
  })

  it('fails as a failure to reach the server when the server has not answered by then: it is tried again', async () => {
    const clock = timeouts()
    silent()
    const out = startIndex(repo, 7, 'abc', 'head')
    clock.pass(0)
    const error = await out.catch((e: unknown) => e)
    expect(error).toBeInstanceOf(ApiError)
    expect(error).toMatchObject({ status: 0, code: 'network' })
  })

  it('has a limit of its own each time it is sent: one that is up does not cut the next short', async () => {
    const clock = timeouts()
    silent()
    const first = startIndex(repo, 7, 'abc', 'head')
    clock.pass(0)
    await first.catch(() => {})
    void startIndex(repo, 7, 'abc', 'head').catch(() => {})
    expect(clock.lasts).toEqual([START_INDEX_TIMEOUT_MS, START_INDEX_TIMEOUT_MS])
  })
})

describe('the stop of an index build', () => {
  it(`is sent with a limit of ${CANCEL_INDEX_TIMEOUT_MS} ms`, () => {
    const clock = timeouts()
    const sent = silent()
    void cancelIndex(repo, 7, 'abc', 'base').catch(() => {})
    expect(sent).toEqual([{ path: '/api/github.com/octo/hello/pulls/7/index?head=abc&side=base', method: 'DELETE' }])
    expect(clock.lasts).toEqual([CANCEL_INDEX_TIMEOUT_MS])
  })

  it('fails as a failure to reach the server when the server has not answered by then', async () => {
    const clock = timeouts()
    silent()
    const out = cancelIndex(repo, 7, 'abc', 'base')
    clock.pass(0)
    const error = await out.catch((e: unknown) => e)
    expect(error).toBeInstanceOf(ApiError)
    expect(error).toMatchObject({ status: 0, code: 'network' })
  })
})
