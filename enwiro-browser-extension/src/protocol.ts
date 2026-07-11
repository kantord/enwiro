// Wire types shared with the native host (`enw browser host`); see
// enwiro/src/commands/browser.rs for the Rust side.

export const NATIVE_HOST_NAME = 'ro.enwi.browser_host'

/// Rule sets with a different version are discarded: the host and the
/// store-distributed extension can be arbitrarily far apart in age.
export const PROTOCOL_VERSION = 1

export interface RuleEntry {
  cookbook: string
  /** Anchored regex claim over recipe names (Rust syntax on the wire). */
  namePattern: string
  /** URLPattern constructor string over page URLs. */
  urlPattern: string
  /** `{group}` template rendered from URL pattern captures. */
  recipeTemplate: string
}

export type HostResponse =
  | { type: 'rules'; version: number; rules: RuleEntry[] }
  | { type: 'activateResult'; ok: boolean; error?: string }
  | { type: 'error'; error: string }
