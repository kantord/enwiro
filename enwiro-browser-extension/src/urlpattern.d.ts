// Minimal declaration for the native URLPattern API (Chrome 95+, Node 24+),
// scoped to what the router uses; drop once the TS standard libs ship it.

interface URLPatternComponentResult {
  input: string
  groups: Record<string, string | undefined>
}

interface URLPatternResult {
  protocol: URLPatternComponentResult
  username: URLPatternComponentResult
  password: URLPatternComponentResult
  hostname: URLPatternComponentResult
  port: URLPatternComponentResult
  pathname: URLPatternComponentResult
  search: URLPatternComponentResult
  hash: URLPatternComponentResult
}

declare class URLPattern {
  constructor(input: string)
  readonly hostname: string
  exec(input: string): URLPatternResult | null
}
