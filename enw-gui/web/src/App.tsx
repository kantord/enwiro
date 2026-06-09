import { Rocket } from 'lucide-react'
import { Button } from '@/components/ui/button'

function App() {
  return (
    <main className="flex min-h-svh flex-col items-center justify-center gap-6 bg-background px-6 text-foreground">
      <div className="flex flex-col items-center gap-3 text-center">
        <span className="inline-flex size-12 items-center justify-center rounded-xl bg-primary text-primary-foreground">
          <Rocket className="size-6" />
        </span>
        <h1 className="text-3xl font-semibold tracking-tight">enw-gui</h1>
        <p className="max-w-md text-sm text-muted-foreground">
          Hello, world — shadcn (Base UI) + Tailwind v4 + Geist, served from a
          single Rust binary.
        </p>
      </div>
      <div className="flex gap-2">
        <Button>Get started</Button>
        <Button variant="outline">Documentation</Button>
      </div>
    </main>
  )
}

export default App
