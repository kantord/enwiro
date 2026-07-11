// The client-side URL router: compiles the host-served rules and resolves
// "the page I am on" to a recipe name. Pure logic, no chrome.* APIs, so it
// is unit-testable in Node (whose native URLPattern matches Chrome's).

import type { RuleEntry } from './protocol'

interface CompiledRule {
  entry: RuleEntry
  urlPattern: URLPattern
  nameClaim: RegExp
}

export interface RouteMatch {
  recipe: string
  cookbook: string
}

/**
 * Rules bucketed by literal hostname so a navigation only tests the
 * current host's rules; rules whose hostname component contains pattern
 * syntax land in a small fallback bucket tested on every navigation.
 */
export class Router {
  private byHost = new Map<string, CompiledRule[]>()
  private wildcardHost: CompiledRule[] = []

  /** Invalid rules are skipped: one bad cookbook must not break routing. */
  constructor(rules: RuleEntry[]) {
    for (const entry of rules) {
      let compiled: CompiledRule
      try {
        compiled = {
          entry,
          urlPattern: new URLPattern(entry.urlPattern),
          nameClaim: rustRegexToJs(entry.namePattern),
        }
      } catch {
        continue
      }
      const hostname = compiled.urlPattern.hostname
      if (isLiteralHostname(hostname)) {
        const bucket = this.byHost.get(hostname) ?? []
        bucket.push(compiled)
        this.byHost.set(hostname, bucket)
      } else {
        this.wildcardHost.push(compiled)
      }
    }
  }

  /**
   * First matching rule wins - the host serves rules in the daemon cache's
   * cookbook-priority order. A rule whose derived name fails its own name
   * claim is treated as a non-match rather than offering a recipe the host
   * would reject.
   */
  match(url: string): RouteMatch | null {
    const hostname = hostnameOf(url)
    const candidates = [
      ...(hostname ? (this.byHost.get(hostname) ?? []) : []),
      ...this.wildcardHost,
    ]
    for (const rule of candidates) {
      const result = rule.urlPattern.exec(url)
      if (!result) {
        continue
      }
      const recipe = renderTemplate(
        rule.entry.recipeTemplate,
        collectGroups(result),
      )
      if (recipe === null || !rule.nameClaim.test(recipe)) {
        continue
      }
      return { recipe, cookbook: rule.entry.cookbook }
    }
    return null
  }
}

function hostnameOf(url: string): string | null {
  try {
    return new URL(url).hostname
  } catch {
    return null
  }
}

/** A hostname with URLPattern syntax in it cannot be used as a map key. */
function isLiteralHostname(hostname: string): boolean {
  return hostname.length > 0 && !/[*:?+()[\]{}\\]/.test(hostname)
}

function collectGroups(result: URLPatternResult): Map<string, string> {
  const groups = new Map<string, string>()
  for (const component of [
    result.protocol,
    result.username,
    result.password,
    result.hostname,
    result.port,
    result.pathname,
    result.search,
    result.hash,
  ]) {
    for (const [key, value] of Object.entries(component.groups)) {
      if (value !== undefined) {
        groups.set(key, value)
      }
    }
  }
  return groups
}

/**
 * Render a `{key}` template (leon syntax on the Rust side: `\{`, `\}` and
 * `\\` escape literal characters). Unknown keys fail the render - a rule
 * whose template references a group the URL did not capture must not
 * produce a half-rendered recipe name.
 */
export function renderTemplate(
  template: string,
  values: Map<string, string>,
): string | null {
  let rendered = ''
  let index = 0
  while (index < template.length) {
    const char = template[index]
    if (char === '\\' && index + 1 < template.length) {
      rendered += template[index + 1]
      index += 2
      continue
    }
    if (char === '{') {
      const end = template.indexOf('}', index)
      if (end === -1) {
        return null
      }
      const value = values.get(template.slice(index + 1, end))
      if (value === undefined) {
        return null
      }
      rendered += value
      index = end + 1
      continue
    }
    rendered += char
    index += 1
  }
  return rendered
}

/**
 * Compile a cache name claim as a JS RegExp. The claims use Rust `regex`
 * syntax; the dialects agree on everything cookbooks realistically emit
 * except named groups (`(?P<n>` vs `(?<n>`). `\(` escapes produced by the
 * Rust-side literal escaping keep a literal `(?P<` from ever hitting this
 * rewrite.
 */
export function rustRegexToJs(pattern: string): RegExp {
  return new RegExp(pattern.replaceAll('(?P<', '(?<'), 'u')
}
