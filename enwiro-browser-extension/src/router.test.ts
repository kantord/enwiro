import { describe, expect, it } from 'vitest'
import type { RuleEntry } from './protocol'
import { Router, renderTemplate } from './router'

// The exact shape the Rust side produces: the github cookbook's URL rule
// (see enwiro-cookbook-github's github_url_rule).
const githubRule: RuleEntry = {
  urlPattern:
    'https://github.com/kantord/enwiro/:kind(pull|issues)/:number([0-9]+){/*}?',
  recipeTemplate: 'enwiro#{number}',
}

describe('Router', () => {
  it('routes PR and issue pages to the derived recipe', () => {
    const router = new Router([githubRule])
    expect(router.match('https://github.com/kantord/enwiro/pull/42')).toBe(
      'enwiro#42',
    )
    expect(router.match('https://github.com/kantord/enwiro/issues/615')).toBe(
      'enwiro#615',
    )
  })

  it('routes subpages and ignores query strings and fragments', () => {
    const router = new Router([githubRule])
    for (const url of [
      'https://github.com/kantord/enwiro/pull/42/files',
      'https://github.com/kantord/enwiro/pull/42?diff=split',
      'https://github.com/kantord/enwiro/pull/42/files#diff-abc123',
    ]) {
      expect(router.match(url)).toBe('enwiro#42')
    }
  })

  it('does not match unrelated pages', () => {
    const router = new Router([githubRule])
    for (const url of [
      'https://github.com/kantord/enwiro',
      'https://github.com/kantord/enwiro/pulls',
      'https://github.com/other/enwiro/pull/42',
      'https://example.com/kantord/enwiro/pull/42',
      'not a url',
    ]) {
      expect(router.match(url)).toBeNull()
    }
  })

  it('first match wins in served order', () => {
    const catchAll: RuleEntry = {
      urlPattern: 'https://github.com/kantord/enwiro/pull/:number([0-9]+)',
      recipeTemplate: 'web-{number}',
    }
    const url = 'https://github.com/kantord/enwiro/pull/1'
    expect(new Router([githubRule, catchAll]).match(url)).toBe('enwiro#1')
    expect(new Router([catchAll, githubRule]).match(url)).toBe('web-1')
  })

  it('skips invalid rules without breaking valid ones', () => {
    const broken: RuleEntry = {
      urlPattern: 'https://github.com/:kind(pull',
      recipeTemplate: 'x',
    }
    const router = new Router([broken, githubRule])
    expect(router.match('https://github.com/kantord/enwiro/pull/7')).toBe(
      'enwiro#7',
    )
  })
})

describe('renderTemplate', () => {
  const values = new Map([['number', '42']])

  it('substitutes captured groups', () => {
    expect(renderTemplate('enwiro#{number}', values)).toBe('enwiro#42')
  })

  it('fails on unknown keys instead of half-rendering', () => {
    expect(renderTemplate('enwiro#{typo}', values)).toBeNull()
    expect(renderTemplate('enwiro#{unclosed', values)).toBeNull()
  })

  it('unescapes leon-style literal braces', () => {
    expect(renderTemplate('app\\{v2\\}#{number}', values)).toBe('app{v2}#42')
    expect(renderTemplate('back\\\\slash', new Map())).toBe('back\\slash')
  })
})
