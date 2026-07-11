// The client-side URL router: compiles the host-served rules and resolves
// "the page I am on" to a recipe name. Pure logic, no chrome.* APIs, so it
// is unit-testable in Node (whose native URLPattern matches Chrome's).

import type { RuleEntry } from './protocol'

interface CompiledRule {
  entry: RuleEntry
  urlPattern: URLPattern
}

/**
 * First matching rule wins - the host serves rules in the daemon cache's
 * cookbook-priority order, and a linear scan preserves it (rule count is
 * one per configured repo; no indexing needed). Invalid rules are skipped:
 * one bad cookbook must not break routing.
 */
export class Router {
  private rules: CompiledRule[] = []

  constructor(rules: RuleEntry[]) {
    for (const entry of rules) {
      try {
        this.rules.push({ entry, urlPattern: new URLPattern(entry.urlPattern) })
      } catch {
        // skipped
      }
    }
  }

  /** The recipe name for `url`, or null when no rule matches. */
  match(url: string): string | null {
    for (const rule of this.rules) {
      let result: URLPatternResult | null
      try {
        result = rule.urlPattern.exec(url)
      } catch {
        continue
      }
      if (!result) {
        continue
      }
      const recipe = renderTemplate(
        rule.entry.recipeTemplate,
        collectGroups(result),
      )
      if (recipe !== null) {
        return recipe
      }
    }
    return null
  }
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
