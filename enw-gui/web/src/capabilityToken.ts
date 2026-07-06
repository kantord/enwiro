import { client } from './client/client.gen'

const STORAGE_KEY = 'enw-gui-token'

/**
 * Thrown (via the error interceptor below) when an API request is rejected
 * for missing/invalid capability token, so callers can show "reopen enw-gui"
 * instead of a generic fetch-failure message.
 */
export class UnauthorizedError extends Error {
  constructor() {
    super('missing or invalid capability token')
    this.name = 'UnauthorizedError'
  }
}

/**
 * Reads the capability token enw-gui minted for this process (see
 * enwiro_sdk::capability, Rust-side) from the URL it was opened with,
 * persists it in sessionStorage so a reload doesn't lose it once stripped
 * from the URL, and attaches it to every API request. Loopback binding and
 * the daemon's Host check don't stop another local process from reaching
 * this port, so every /api call must carry this token.
 */
export function initCapabilityToken(): void {
  const url = new URL(window.location.href)
  const token =
    url.searchParams.get('token') ?? sessionStorage.getItem(STORAGE_KEY)

  if (token) {
    sessionStorage.setItem(STORAGE_KEY, token)
    client.setConfig({ headers: { Authorization: `Bearer ${token}` } })
  }

  if (url.searchParams.has('token')) {
    url.searchParams.delete('token')
    window.history.replaceState({}, '', url)
  }

  // A 401 here means the token is missing/stale (e.g. this tab outlived an
  // enw-gui restart, which mints a fresh one) — a different problem, with a
  // different fix, than any other fetch failure. Surface it as its own error
  // type so callers can tell it apart instead of showing a generic message.
  client.interceptors.error.use((error, response) => {
    if (response?.status === 401) {
      return new UnauthorizedError()
    }
    return error
  })
}
