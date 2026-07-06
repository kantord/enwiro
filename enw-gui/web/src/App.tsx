import {
  DndContext,
  type DragEndEvent,
  DragOverlay,
  type DragStartEvent,
  useDraggable,
  useDroppable,
} from '@dnd-kit/core'
import { useQuery } from '@tanstack/react-query'
import { MoreHorizontal } from 'lucide-react'
import { type ReactNode, useEffect, useRef, useState } from 'react'
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
import { Skeleton } from '@/components/ui/skeleton'
import { G_ACTIONS, useKeyboardControls } from '@/keyboard'
import { cn } from '@/lib/utils'
import { CARD_RENDER_CAP, type Mode, useBoardStore } from '@/store'
import { UnauthorizedError } from './capabilityToken'

function BoardCard({ card }: { card: CardData }) {
  const moveCard = useBoardStore((s) => s.moveCard)
  return (
    <Card className="gap-2 py-3">
      <CardHeader className="px-3">
        <div className="flex items-start justify-between gap-2">
          <CardTitle className="break-all text-sm">{card.name}</CardTitle>
          <DropdownMenu>
            <DropdownMenuTrigger
              aria-label="Card actions"
              onPointerDown={(e) => e.stopPropagation()}
              className="inline-flex size-6 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50"
            >
              <MoreHorizontal className="size-4" />
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuLabel>Actions</DropdownMenuLabel>
              {G_ACTIONS.map((a) => (
                <DropdownMenuItem
                  key={a.status}
                  onClick={() => moveCard(card.name, a.status)}
                >
                  {a.label}
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
  colIdx,
  rowIdx,
}: {
  card: CardData
  fromKey: string
  colIdx: number
  rowIdx: number
}) {
  const { setNodeRef, attributes, listeners, isDragging } = useDraggable({
    id: card.name,
    data: { fromKey },
  })
  const selected = useBoardStore(
    (s) => s.selected.col === colIdx && s.selected.row === rowIdx,
  )
  const mode = useBoardStore((s) => s.mode)
  const select = useBoardStore((s) => s.select)
  const elRef = useRef<HTMLDivElement | null>(null)

  useEffect(() => {
    if (selected) {
      elRef.current?.scrollIntoView({ block: 'nearest' })
    }
  }, [selected])

  return (
    <div
      ref={(el) => {
        setNodeRef(el)
        elRef.current = el
      }}
      {...attributes}
      {...listeners}
      onPointerDownCapture={() => select({ col: colIdx, row: rowIdx })}
      className={cn(
        'shrink-0 cursor-grab rounded-xl active:cursor-grabbing',
        selected &&
          (mode === 'move'
            ? 'shadow-md ring-2 ring-primary'
            : 'ring-2 ring-primary/50'),
        isDragging && 'opacity-40',
      )}
    >
      <BoardCard card={card} />
    </div>
  )
}

function Column({ col, colIdx }: { col: BoardColumn; colIdx: number }) {
  const { setNodeRef, isOver } = useDroppable({ id: col.key })
  return (
    <section className="flex min-h-0 flex-col gap-3 rounded-xl border bg-muted/30 p-3">
      <header className="flex shrink-0 items-center justify-between px-1">
        <h2 className="font-medium text-sm">{col.title}</h2>
        <Badge variant="secondary">{col.cards.length}</Badge>
      </header>
      <div
        ref={setNodeRef}
        className={cn(
          'flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto rounded-lg transition-colors',
          isOver && 'bg-accent',
        )}
      >
        {col.cards.length === 0 ? (
          <div className="m-1 rounded-lg border border-dashed p-6 text-center text-muted-foreground text-xs">
            Drop here
          </div>
        ) : (
          <>
            {col.cards.slice(0, CARD_RENDER_CAP).map((card, rowIdx) => (
              <DraggableCard
                key={card.name}
                card={card}
                fromKey={col.key}
                colIdx={colIdx}
                rowIdx={rowIdx}
              />
            ))}
            {col.cards.length > CARD_RENDER_CAP ? (
              <p className="shrink-0 px-1 py-2 text-center text-muted-foreground text-xs">
                +{col.cards.length - CARD_RENDER_CAP} more
              </p>
            ) : null}
          </>
        )}
      </div>
    </section>
  )
}

function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="rounded border bg-muted px-1.5 py-0.5 font-medium text-[10px] text-muted-foreground">
      {children}
    </kbd>
  )
}

const MODE_LABELS: Record<Mode, string> = {
  normal: 'normal',
  g: 'action',
  move: 'move',
}

function KeyBar() {
  const mode = useBoardStore((s) => s.mode)
  return (
    <footer className="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-1 border-t px-6 py-2 text-muted-foreground text-xs">
      <Badge
        variant={mode === 'normal' ? 'secondary' : 'default'}
        className="uppercase"
      >
        {MODE_LABELS[mode]}
      </Badge>
      {mode === 'normal' ? (
        <>
          <span>
            <Kbd>←↑↓→</Kbd> <Kbd>hjkl</Kbd> <Kbd>wasd</Kbd> select
          </span>
          <span>
            <Kbd>g</Kbd> actions
          </span>
          <span>
            <Kbd>e</Kbd> move mode
          </span>
        </>
      ) : null}
      {mode === 'g' ? (
        <>
          {G_ACTIONS.map((a) => (
            <span key={a.key}>
              <Kbd>{a.key}</Kbd> {a.label}
            </span>
          ))}
          <span>
            <Kbd>esc</Kbd> cancel
          </span>
        </>
      ) : null}
      {mode === 'move' ? (
        <>
          <span>
            <Kbd>←/→</Kbd> <Kbd>h/l</Kbd> <Kbd>a/d</Kbd> move card
          </span>
          <span>
            <Kbd>esc</Kbd> / <Kbd>enter</Kbd> done
          </span>
        </>
      ) : null}
    </footer>
  )
}

function BoardSkeleton() {
  return (
    <div className="grid min-h-0 flex-1 grid-cols-1 gap-4 overflow-hidden p-6 sm:grid-cols-2 lg:grid-cols-4">
      {G_ACTIONS.map((a) => (
        <section
          key={a.status}
          className="flex min-h-0 flex-col gap-3 rounded-xl border bg-muted/30 p-3"
        >
          <Skeleton className="h-6 w-24" />
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </section>
      ))}
    </div>
  )
}

function Centered({ children }: { children: ReactNode }) {
  return (
    <div className="flex flex-1 items-center justify-center px-6 text-center text-muted-foreground text-sm">
      {children}
    </div>
  )
}

function App() {
  const board = useQuery(getBoardOptions())
  const columns = useBoardStore((s) => s.columns)
  const seed = useBoardStore((s) => s.seed)
  const moveCard = useBoardStore((s) => s.moveCard)
  const [activeCard, setActiveCard] = useState<CardData | null>(null)

  useKeyboardControls()

  useEffect(() => {
    if (board.data) {
      seed(board.data.columns)
    }
  }, [board.data, seed])

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
      moveCard(String(active.id), toKey)
    }
  }

  return (
    <DndContext
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onDragCancel={() => setActiveCard(null)}
    >
      <main className="flex h-svh flex-col bg-background text-foreground">
        <header className="flex shrink-0 items-baseline gap-3 border-b px-6 py-4">
          <h1 className="font-semibold text-lg tracking-tight">enwiro</h1>
          <p className="text-muted-foreground text-sm">environments</p>
        </header>
        {board.isError ? (
          <Centered>
            {board.error instanceof UnauthorizedError
              ? 'Session expired. Restart enw-gui and open the link it prints.'
              : "Couldn't load the board. Is the enwiro daemon running?"}
          </Centered>
        ) : columns === null ? (
          <BoardSkeleton />
        ) : (
          <div className="grid min-h-0 flex-1 grid-cols-1 gap-4 overflow-hidden p-6 sm:grid-cols-2 lg:grid-cols-4">
            {columns.map((col, colIdx) => (
              <Column key={col.key} col={col} colIdx={colIdx} />
            ))}
          </div>
        )}
        <KeyBar />
      </main>
      <DragOverlay>
        {activeCard ? <BoardCard card={activeCard} /> : null}
      </DragOverlay>
    </DndContext>
  )
}

export default App
