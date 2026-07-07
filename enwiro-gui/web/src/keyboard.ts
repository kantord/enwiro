import { useEffect } from 'react'
import { useBoardStore } from '@/store'

/** g-mode action keys. Phrased as actions (mark as done), not as moves; keys
 * match the column initials so they are easy to associate. Also reused by the
 * card dropdown menu so both inputs share one vocabulary. */
export const G_ACTIONS = [
  { key: 'r', status: 'ready', label: 'Mark ready' },
  { key: 'a', status: 'active', label: 'Work on' },
  { key: 'w', status: 'waiting', label: 'Mark waiting' },
  { key: 'd', status: 'done', label: 'Mark done' },
]

const LEFT = ['ArrowLeft', 'h', 'a']
const RIGHT = ['ArrowRight', 'l', 'd']
const UP = ['ArrowUp', 'k', 'w']
const DOWN = ['ArrowDown', 'j', 's']

/** Global keyboard controls, hand-rolled on a single `keydown` listener.
 * Deliberately not a hotkey library: the g-command needs a *pending* mode the
 * UI renders (keybar shows the next keys), and the vim-style modes already
 * live in the zustand store, so a plain listener reading store state is both
 * the web-standard and the simplest correct implementation. */
export function useKeyboardControls() {
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey) return
      const target = e.target as HTMLElement | null
      // Leave typing contexts and open popups (Base UI menus handle their own
      // keyboard interaction) alone.
      if (
        target?.closest(
          'input, textarea, select, [contenteditable="true"], [role="menu"], [role="dialog"]',
        )
      ) {
        return
      }
      // dnd-kit keyboard drag in progress (Space-lifted card): let it own the
      // arrow keys.
      if (target?.getAttribute('aria-pressed') === 'true') return

      const store = useBoardStore.getState()
      let handled = true

      if (store.mode === 'g') {
        const action = G_ACTIONS.find((a) => a.key === e.key)
        if (action) {
          store.setStatusSelected(action.status)
        }
        // Any key leaves g-mode; invalid keys simply cancel.
        store.setMode('normal')
      } else if (store.mode === 'move') {
        if (LEFT.includes(e.key)) store.moveSelected(-1)
        else if (RIGHT.includes(e.key)) store.moveSelected(1)
        else if (e.key === 'Escape' || e.key === 'Enter' || e.key === 'e')
          store.setMode('normal')
        else handled = UP.includes(e.key) || DOWN.includes(e.key)
      } else {
        if (LEFT.includes(e.key)) store.navigate(-1, 0)
        else if (RIGHT.includes(e.key)) store.navigate(1, 0)
        else if (UP.includes(e.key)) store.navigate(0, -1)
        else if (DOWN.includes(e.key)) store.navigate(0, 1)
        else if (e.key === 'g') store.setMode('g')
        else if (e.key === 'e') store.setMode('move')
        else handled = false
      }

      if (handled) e.preventDefault()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [])
}
