import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { MoreHorizontal } from 'lucide-react'
import type { ReactNode } from 'react'
import type { Card as CardData } from '@/client'
import {
  getBoardOptions,
  postMarkMutation,
} from '@/client/@tanstack/react-query.gen'
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
}: {
  card: CardData
  onSetStatus: (envName: string, status: string) => void
}) {
  return (
    <Card className="gap-2 py-3">
      <CardHeader className="px-3">
        <div className="flex items-start justify-between gap-2">
          <CardTitle className="text-sm break-all">{card.name}</CardTitle>
          <DropdownMenu>
            <DropdownMenuTrigger
              aria-label="Set status"
              className="inline-flex size-6 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50"
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

function App() {
  const queryClient = useQueryClient()
  const board = useQuery(getBoardOptions())
  const mark = useMutation({
    ...postMarkMutation(),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: getBoardOptions().queryKey }),
  })

  const onSetStatus = (envName: string, status: string) =>
    mark.mutate({ body: { env_name: envName, status } })

  if (board.isPending) {
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
    <main className="min-h-svh bg-background p-6 text-foreground">
      <h1 className="mb-6 text-2xl font-semibold tracking-tight">enwiro</h1>
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
        {board.data.columns.map((col) => (
          <section key={col.key} className="flex flex-col gap-3">
            <header className="flex items-center justify-between px-1">
              <h2 className="text-sm font-medium text-muted-foreground">
                {col.title}
              </h2>
              <span className="text-xs text-muted-foreground">
                {col.cards.length}
              </span>
            </header>
            <div className="flex flex-col gap-2">
              {col.cards.map((card) => (
                <BoardCard
                  key={card.name}
                  card={card}
                  onSetStatus={onSetStatus}
                />
              ))}
            </div>
          </section>
        ))}
      </div>
    </main>
  )
}

export default App
