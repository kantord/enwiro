// MV3 service worker: keeps a rule copy pulled from the native host,
// badges the toolbar action when the current page routes to a recipe, and
// activates that recipe on click. All validation and activation logic
// lives host-side; see enwiro/src/commands/browser.rs for why.

import type { HostRequest, HostResponse, RuleEntry } from './protocol'
import { NATIVE_HOST_NAME, PROTOCOL_VERSION } from './protocol'
import { Router } from './router'

/** How long a pulled rule set stays fresh. Rules change when cookbooks or
 * their config change, so staleness costs at most a briefly missing badge. */
const RULES_TTL_MS = 30 * 60 * 1000

interface StoredRules {
  rules: RuleEntry[]
  fetchedAtEpochMs: number
}

/** In-worker cache of the compiled router; rebuilt when the stored rules
 * pass their TTL, not just when MV3 tears the worker down. */
let compiled: { router: Router; fetchedAtEpochMs: number } | null = null

function sendToHost(message: HostRequest): Promise<HostResponse> {
  return chrome.runtime.sendNativeMessage(
    NATIVE_HOST_NAME,
    message,
  ) as Promise<HostResponse>
}

async function fetchRules(): Promise<RuleEntry[]> {
  const response = await sendToHost({ type: 'getRules' })
  if (response.type === 'error') {
    throw new Error(response.error)
  }
  if (response.type !== 'rules' || response.version !== PROTOCOL_VERSION) {
    throw new Error(
      'Host speaks an unknown protocol version; update enwiro or the extension',
    )
  }
  return response.rules
}

function isFresh(fetchedAtEpochMs: number): boolean {
  return Date.now() - fetchedAtEpochMs <= RULES_TTL_MS
}

/**
 * The compiled router over the freshest rules available: memo while fresh,
 * else re-pull from the host, else fall back to the stored (stale) copy. A
 * failed pull with nothing stored yields an empty router that is NOT
 * memoized, so the next navigation retries instead of pinning "no rules"
 * for the worker's lifetime (e.g. browser started before the daemon).
 */
async function getRouter(): Promise<Router> {
  if (compiled && isFresh(compiled.fetchedAtEpochMs)) {
    return compiled.router
  }
  const stored = (await chrome.storage.local.get('rules')) as {
    rules?: StoredRules
  }
  let rules = stored.rules
  if (!rules || !isFresh(rules.fetchedAtEpochMs)) {
    try {
      rules = { rules: await fetchRules(), fetchedAtEpochMs: Date.now() }
      await chrome.storage.local.set({ rules })
    } catch (e) {
      console.warn('enwiro: could not refresh rules from the native host', e)
    }
  }
  if (!rules) {
    return new Router([])
  }
  compiled = {
    router: new Router(rules.rules),
    fetchedAtEpochMs: rules.fetchedAtEpochMs,
  }
  return compiled.router
}

async function updateBadge(
  tabId: number,
  url: string | undefined,
): Promise<void> {
  const recipe = url ? (await getRouter()).match(url) : null
  if (recipe) {
    await chrome.action.setBadgeText({ tabId, text: 'env' })
    await chrome.action.setTitle({
      tabId,
      title: `Activate ${recipe} in enwiro`,
    })
  } else {
    await chrome.action.setBadgeText({ tabId, text: '' })
    await chrome.action.setTitle({
      tabId,
      title: 'enwiro: no environment for this page',
    })
  }
}

async function refreshTab(tabId: number): Promise<void> {
  try {
    const tab = await chrome.tabs.get(tabId)
    await updateBadge(tabId, tab.url)
  } catch {
    // Tab gone, or a page we cannot read; nothing to badge.
  }
}

async function activateForTab(tab: chrome.tabs.Tab): Promise<void> {
  const recipe = tab.url ? (await getRouter()).match(tab.url) : null
  if (!recipe) {
    return
  }
  const notificationId = `enwiro-activate-${Date.now()}`
  await chrome.notifications.create(notificationId, {
    type: 'basic',
    iconUrl: 'icon.png',
    title: 'enwiro',
    message: `Activating ${recipe}...`,
  })
  try {
    const response = await sendToHost({ type: 'activate', recipe })
    if (response.type === 'activated') {
      // Success needs no follow-up: the workspace switch is the feedback.
      return
    }
    const reason =
      response.type === 'error'
        ? response.error
        : `unexpected host response '${response.type}'`
    await chrome.notifications.update(notificationId, {
      title: 'enwiro',
      message: `Failed to activate ${recipe}: ${reason}`,
    })
  } catch (e) {
    // The host did not even start: manifest missing or enwiro not installed.
    await chrome.notifications.update(notificationId, {
      title: 'enwiro',
      message: `Could not reach enwiro (${e instanceof Error ? e.message : e}). Is enwiro installed? Try: enw browser install`,
    })
  }
}

chrome.tabs.onUpdated.addListener((tabId, changeInfo, tab) => {
  if (changeInfo.url || changeInfo.status === 'complete') {
    void updateBadge(tabId, tab.url)
  }
})

chrome.tabs.onActivated.addListener(({ tabId }) => {
  void refreshTab(tabId)
})

// SPA navigations (GitHub navigates via pushState) change the URL without
// a load; onUpdated does not always fire for them.
chrome.webNavigation.onHistoryStateUpdated.addListener((details) => {
  if (details.frameId === 0) {
    void updateBadge(details.tabId, details.url)
  }
})

chrome.action.onClicked.addListener((tab) => {
  void activateForTab(tab)
})
