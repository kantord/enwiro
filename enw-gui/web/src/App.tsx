// Minimal dnd-kit proof. No scroll, no API, no shadcn/Tailwind, no DragOverlay,
// no custom sensors. If this doesn't drag by hand, the problem is environmental.
import {
  DndContext,
  type DragEndEvent,
  useDraggable,
  useDroppable,
} from '@dnd-kit/core'
import { CSS } from '@dnd-kit/utilities'
import { type ReactNode, useState } from 'react'

const COLUMNS = ['Todo', 'Doing', 'Done']
const INITIAL: Record<string, string> = {
  'Card A': 'Todo',
  'Card B': 'Todo',
  'Card C': 'Doing',
  'Card D': 'Done',
}

function Draggable({ id }: { id: string }) {
  const { attributes, listeners, setNodeRef, transform, isDragging } =
    useDraggable({ id })
  return (
    <button
      type="button"
      ref={setNodeRef}
      {...listeners}
      {...attributes}
      style={{
        display: 'block',
        width: '100%',
        margin: '6px 0',
        padding: 10,
        border: '1px solid #ccc',
        borderRadius: 6,
        background: '#fff',
        textAlign: 'left',
        cursor: 'grab',
        opacity: isDragging ? 0.4 : 1,
        transform: CSS.Translate.toString(transform),
      }}
    >
      {id}
    </button>
  )
}

function Droppable({ id, children }: { id: string; children: ReactNode }) {
  const { isOver, setNodeRef } = useDroppable({ id })
  return (
    <div
      ref={setNodeRef}
      style={{
        flex: 1,
        minWidth: 160,
        padding: 12,
        border: '1px solid #ddd',
        borderRadius: 8,
        background: isOver ? '#eef2ff' : '#fafafa',
      }}
    >
      <strong>{id}</strong>
      {children}
    </div>
  )
}

export default function App() {
  const [colOf, setColOf] = useState<Record<string, string>>(INITIAL)
  const onDragEnd = (e: DragEndEvent) => {
    const { active, over } = e
    if (over) {
      setColOf((m) => ({ ...m, [String(active.id)]: String(over.id) }))
    }
  }
  return (
    <DndContext onDragEnd={onDragEnd}>
      <div
        style={{
          display: 'flex',
          gap: 16,
          padding: 24,
          fontFamily: 'sans-serif',
        }}
      >
        {COLUMNS.map((col) => (
          <Droppable key={col} id={col}>
            {Object.keys(colOf)
              .filter((card) => colOf[card] === col)
              .map((card) => (
                <Draggable key={card} id={card} />
              ))}
          </Droppable>
        ))}
      </div>
    </DndContext>
  )
}
