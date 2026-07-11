// MV3 service worker: keeps a rule copy pulled from the native host,
// badges the toolbar action when the current page routes to a recipe, and
// activates that recipe on click. Deliberately dumb - all validation and
// activation logic lives host-side (see enwiro/src/commands/browser.rs),
// because the extension ships on a store-review cadence.

import type { HostResponse, RuleEntry } from './protocol'
import { NATIVE_HOST_NAME, PROTOCOL_VERSION } from './protocol'
import { Router } from './router'

/** How long a pulled rule set stays fresh. Rules change when cookbooks or
 * their config change, so staleness costs at most a briefly missing badge. */
const RULES_TTL_MS = 30 * 60 * 1000

interface StoredRules {
  rules: RuleEntry[]
  fetchedAt: number
}

let router: Router | null = null

function sendToHost(message: object): Promise<HostResponse> {
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

async function getRouter(): Promise<Router> {
  if (router) {
    return router
  }
  const stored = (await chrome.storage.local.get('rules')) as {
    rules?: StoredRules
  }
  let rules = stored.rules
  if (!rules || Date.now() - rules.fetchedAt > RULES_TTL_MS) {
    try {
      rules = { rules: await fetchRules(), fetchedAt: Date.now() }
      await chrome.storage.local.set({ rules })
    } catch (e) {
      console.warn('enwiro: could not refresh rules from the native host', e)
      // A stale copy still routes; only a missing one leaves us empty.
    }
  }
  router = new Router(rules?.rules ?? [])
  return router
}

async function updateBadge(
  tabId: number,
  url: string | undefined,
): Promise<void> {
  const match = url ? (await getRouter()).match(url) : null
  if (match) {
    await chrome.action.setBadgeText({ tabId, text: 'env' })
    await chrome.action.setTitle({
      tabId,
      title: `Activate ${match.recipe} in enwiro`,
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
  const match = tab.url ? (await getRouter()).match(tab.url) : null
  if (!match) {
    return
  }
  const notificationId = `enwiro-activate-${Date.now()}`
  await chrome.notifications.create(notificationId, {
    type: 'basic',
    iconUrl: 'icon.png',
    title: 'enwiro',
    message: `Activating ${match.recipe}...`,
  })
  try {
    const response = await sendToHost({
      type: 'activate',
      recipe: match.recipe,
    })
    if (response.type === 'activateResult' && response.ok) {
      // Success needs no follow-up: the workspace switch is the feedback.
      return
    }
    const error =
      response.type === 'activateResult'
        ? (response.error ?? 'unknown error')
        : response.type === 'error'
          ? response.error
          : 'unexpected host response'
    await chrome.notifications.update(notificationId, {
      title: 'enwiro',
      message: `Failed to activate ${match.recipe}: ${error}`,
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

// Drop the cached copy so the next navigation re-pulls: extension updates
// and browser restarts are natural refresh points.
chrome.runtime.onInstalled.addListener(() => {
  void chrome.storage.local.remove('rules')
})
chrome.runtime.onStartup.addListener(() => {
  void chrome.storage.local.remove('rules')
})
