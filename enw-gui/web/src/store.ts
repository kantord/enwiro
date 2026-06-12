import { create } from 'zustand'
import type { BoardColumn, Card } from '@/client'

/** Interaction modes, vim-style: `normal` navigates the selection, `g` waits
 * for an action key (mark ready/done/...), `move` relocates the selected card. */
export type Mode = 'normal' | 'g' | 'move'

/** Temporary cap: mounting all 4339 "Ready" cards creates as many useDraggable
 * subscribers and every real mousemove re-renders them all, freezing the drag.
 * Confirmed by hand-testing. Real fix: virtualization or trimming the recipe
 * list; until then navigation also clamps to the rendered subset. */
export const CARD_RENDER_CAP = 25

export interface Selection {
  col: number
  row: number
}

interface BoardState {
  columns: BoardColumn[] | null
  selected: Selection
  mode: Mode
  seed: (columns: BoardColumn[]) => void
  select: (selected: Selection) => void
  setMode: (mode: Mode) => void
  navigate: (dCol: number, dRow: number) => void
  moveCard: (name: string, toKey: string) => void
  setStatusSelected: (statusKey: string) => void
  moveSelected: (dCol: number) => void
}

const visibleCount = (col: BoardColumn) =>
  Math.min(col.cards.length, CARD_RENDER_CAP)

/** Same ordering the backend uses (board.rs::board_order): frecency
 * descending, envs before recipes on ties, then name. Keeping the comparator
 * in sync means a moved card lands where a fresh board read would put it. */
const byBoardOrder = (a: Card, b: Card) =>
  b.score - a.score ||
  Number(a.is_recipe) - Number(b.is_recipe) ||
  a.name.localeCompare(b.name)

/** Move a card to the `toKey` column (FE-local only; nothing is written to the
 * backend yet). Returns the card's new position so the selection can follow. */
function relocate(
  columns: BoardColumn[],
  name: string,
  toKey: string,
): { columns: BoardColumn[]; to: Selection | null } {
  let moved: Card | undefined
  const without = columns.map((col) => {
    const idx = col.cards.findIndex((c) => c.name === name)
    if (idx < 0) return col
    moved = col.cards[idx]
    return { ...col, cards: col.cards.filter((_, i) => i !== idx) }
  })
  if (!moved) return { columns, to: null }
  const card = moved
  let to: Selection | null = null
  const next = without.map((col, colIdx) => {
    if (col.key !== toKey) return col
    const cards = [...col.cards, card].sort(byBoardOrder)
    to = {
      col: colIdx,
      row: Math.min(
        cards.findIndex((c) => c.name === name),
        CARD_RENDER_CAP - 1,
      ),
    }
    return { ...col, cards }
  })
  return { columns: next, to }
}

export const useBoardStore = create<BoardState>((set, get) => ({
  columns: null,
  selected: { col: 0, row: 0 },
  mode: 'normal',

  seed: (columns) => {
    if (get().columns === null) {
      set({ columns })
    }
  },

  select: (selected) => set({ selected }),

  setMode: (mode) => set({ mode }),

  navigate: (dCol, dRow) => {
    const { columns, selected } = get()
    if (!columns || columns.length === 0) return
    const col = Math.min(Math.max(selected.col + dCol, 0), columns.length - 1)
    const maxRow = Math.max(visibleCount(columns[col]) - 1, 0)
    const row = Math.min(Math.max(selected.row + dRow, 0), maxRow)
    set({ selected: { col, row } })
  },

  moveCard: (name, toKey) => {
    const { columns } = get()
    if (!columns) return
    set({ columns: relocate(columns, name, toKey).columns })
  },

  setStatusSelected: (statusKey) => {
    const { columns, selected } = get()
    if (!columns) return
    const name = columns[selected.col]?.cards[selected.row]?.name
    if (!name) return
    const { columns: next, to } = relocate(columns, name, statusKey)
    set(to ? { columns: next, selected: to } : { columns: next })
  },

  moveSelected: (dCol) => {
    const { columns, selected } = get()
    if (!columns) return
    const target = columns[selected.col + dCol]
    if (!target) return
    get().setStatusSelected(target.key)
  },
}))
