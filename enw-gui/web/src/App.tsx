import {
  DndContext,
  type DragEndEvent,
  DragOverlay,
  type DragStartEvent,
  MouseSensor,
  TouchSensor,
  useDraggable,
  useDroppable,
  useSensor,
  useSensors,
} from '@dnd-kit/core'
import { useQuery } from '@tanstack/react-query'
import { GripVertical, MoreHorizontal } from 'lucide-react'
import { type ReactNode, useEffect, useState } from 'react'
import type { BoardColumn, Card as CardData } from '@/client'
import { getBoardOptions } from '@/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'

const STATUSES = [
  { key: 'ready', label: 'Ready' },
  { key: 'active', label: 'Active' },
  { key: 'waiting', label: 'Waiting' },
  { key: 'done', label: 'Done' },
]

/** Move a card to `toKey`, keeping each column sorted by name. Pure FE state;
 * BE persistence is deferred (optimistic + rollback comes later). */
function moveCard(
  columns: BoardColumn[],
  cardName: string,
  toKey: string,
): BoardColumn[] {
  let moved: CardData | undefined
  const without = columns.map((col) => {
    const idx = col.cards.findIndex((c) => c.name === cardName)
    if (idx < 0) return col
    moved = col.cards[idx]
    return { ...col, cards: col.cards.filter((_, i) => i !== idx) }
  })
  if (!moved) return columns
  const card = moved
  return without.map((col) =>
    col.key === toKey
      ? {
          ...col,
          cards: [...col.cards, card].sort((a, b) =>
            a.name.localeCompare(b.name),
          ),
        }
      : col,
  )
}

function Centered({ children }: { children: ReactNode }) {
  return (
    <div className="flex min-h-svh items-center justify-center px-6 text-center text-sm text-muted-foreground">
      {children}
    </div>
  )
}

function BoardCard({
  card,
  onSetStatus,
  dragHandle,
}: {
  card: CardData
  onSetStatus: (envName: string, status: string) => void
  dragHandle?: ReactNode
}) {
  return (
    <Card className="shrink-0 gap-2 py-3">
      <CardHeader className="px-3">
        <div className="flex items-start gap-1">
          {dragHandle}
          <CardTitle className="flex-1 text-sm break-all">
            {card.name}
          </CardTitle>
          <DropdownMenu>
            <DropdownMenuTrigger
              aria-label="Set status"
              className="ml-auto inline-flex size-6 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50"
            >
              <MoreHorizontal className="size-4" />
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuLabel>Set status</DropdownMenuLabel>
              {STATUSES.map((s) => (
                <DropdownMenuItem
                  key={s.key}
                  onClick={() => onSetStatus(card.name, s.key)}
                >
                  {s.label}
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
        {card.description ? (
          <CardDescription className="text-xs">
            {card.description}
          </CardDescription>
        ) : null}
        {card.is_recipe ? (
          <Badge variant="secondary" className="w-fit">
            recipe
          </Badge>
        ) : null}
      </CardHeader>
    </Card>
  )
}

function DraggableCard({
  card,
  fromKey,
  onSetStatus,
}: {
  card: CardData
  fromKey: string
  onSetStatus: (envName: string, status: string) => void
}) {
  const { setNodeRef, attributes, listeners, isDragging } = useDraggable({
    id: card.name,
    data: { fromKey },
  })
  // Listeners live on a dedicated grip handle (not the whole card) — the
  // standard dnd-kit pattern; dragging only starts from the handle, so card
  // text/clicks/the menu never conflict with the gesture.
  const handle = (
    <button
      type="button"
      aria-label="Drag to move"
      {...attributes}
      {...listeners}
      className="-ml-1 flex size-6 shrink-0 cursor-grab touch-none items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-muted hover:text-foreground active:cursor-grabbing"
    >
      <GripVertical className="size-4" />
    </button>
  )
  return (
    <div ref={setNodeRef} className={isDragging ? 'opacity-40' : ''}>
      <BoardCard card={card} onSetStatus={onSetStatus} dragHandle={handle} />
    </div>
  )
}

function DroppableColumn({
  colKey,
  children,
}: {
  colKey: string
  children: ReactNode
}) {
  const { setNodeRef, isOver } = useDroppable({ id: colKey })
  return (
    <div
      ref={setNodeRef}
      className={`flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto rounded-md pr-1 transition-colors ${
        isOver ? 'bg-muted/60' : ''
      }`}
    >
      {children}
    </div>
  )
}

function App() {
  const board = useQuery(getBoardOptions())
  // Local, client-side board state — the source of truth for what's rendered.
  // Seeded once from the server read; moves happen here instantly (no BE yet).
  const [columns, setColumns] = useState<BoardColumn[] | null>(null)
  useEffect(() => {
    if (board.data && columns === null) {
      setColumns(board.data.columns)
    }
  }, [board.data, columns])

  const [activeCard, setActiveCard] = useState<CardData | null>(null)
  // MouseSensor (not PointerSensor): a real mouse can trigger `pointercancel`
  // (native selection/drag takeover), which aborts PointerSensor mid-drag —
  // the "cursor sticks, card frozen" bug. Mouse/Touch events aren't canceled
  // that way. Distance/delay so clicks + the ⋯ menu still work.
  const sensors = useSensors(
    useSensor(MouseSensor, { activationConstraint: { distance: 5 } }),
    useSensor(TouchSensor, {
      activationConstraint: { delay: 150, tolerance: 8 },
    }),
  )

  const onSetStatus = (envName: string, status: string) =>
    setColumns((cols) => (cols ? moveCard(cols, envName, status) : cols))

  const onDragStart = (event: DragStartEvent) => {
    const name = String(event.active.id)
    const card = columns?.flatMap((c) => c.cards).find((c) => c.name === name)
    setActiveCard(card ?? null)
  }
  const onDragEnd = (event: DragEndEvent) => {
    setActiveCard(null)
    const { active, over } = event
    if (!over) return
    const fromKey = (active.data.current as { fromKey?: string })?.fromKey
    const toKey = String(over.id)
    if (fromKey && fromKey !== toKey) {
      onSetStatus(String(active.id), toKey)
    }
  }

  if (board.isPending || columns === null) {
    return <Centered>Loading board…</Centered>
  }
  if (board.isError) {
    return (
      <Centered>
        Couldn't load the board. Is the enwiro daemon running?
      </Centered>
    )
  }

  return (
    <DndContext
      sensors={sensors}
      // Off on purpose: the per-column `overflow-y-auto` containers make
      // dnd-kit's auto-scroll latch onto the huge "Ready" column and freeze
      // real (continuous-pointer) drags. We don't need drag-to-scroll here.
      autoScroll={false}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onDragCancel={() => setActiveCard(null)}
    >
      <main className="flex h-svh flex-col bg-background text-foreground">
        <h1 className="shrink-0 px-6 pt-6 pb-4 text-2xl font-semibold tracking-tight">
          enwiro
        </h1>
        <div className="grid min-h-0 flex-1 grid-cols-1 gap-4 overflow-hidden px-6 pb-6 sm:grid-cols-2 lg:grid-cols-4">
          {columns.map((col) => (
            <section key={col.key} className="flex min-h-0 flex-col gap-3">
              <header className="flex shrink-0 items-center justify-between px-1">
                <h2 className="text-sm font-medium text-muted-foreground">
                  {col.title}
                </h2>
                <span className="text-xs text-muted-foreground">
                  {col.cards.length}
                </span>
              </header>
              <DroppableColumn colKey={col.key}>
                {col.cards.map((card) => (
                  <DraggableCard
                    key={card.name}
                    card={card}
                    fromKey={col.key}
                    onSetStatus={onSetStatus}
                  />
                ))}
              </DroppableColumn>
            </section>
          ))}
        </div>
      </main>
      <DragOverlay>
        {activeCard ? (
          <BoardCard card={activeCard} onSetStatus={() => {}} />
        ) : null}
      </DragOverlay>
    </DndContext>
  )
}

export default App
