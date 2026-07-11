import { describe, expect, it } from 'vitest'
import type { RuleEntry } from './protocol'
import { Router, renderTemplate, rustRegexToJs } from './router'

// The exact shapes the Rust side produces: the github cookbook's URL rule
// (see enwiro-cookbook-github) and the daemon's anchored Rust-syntax name
// claim.
const githubRule: RuleEntry = {
  cookbook: 'github',
  namePattern: '^(?:enwiro#(?P<number>[0-9]{1,19}))$',
  urlPattern:
    'https://github.com/kantord/enwiro/:kind(pull|issues)/:number([0-9]+){/*}?',
  recipeTemplate: 'enwiro#{number}',
}

describe('Router', () => {
  it('routes PR and issue pages to the derived recipe', () => {
    const router = new Router([githubRule])
    expect(router.match('https://github.com/kantord/enwiro/pull/42')).toEqual({
      recipe: 'enwiro#42',
      cookbook: 'github',
    })
    expect(
      router.match('https://github.com/kantord/enwiro/issues/615')?.recipe,
    ).toBe('enwiro#615')
  })

  it('routes subpages and ignores query strings and fragments', () => {
    const router = new Router([githubRule])
    for (const url of [
      'https://github.com/kantord/enwiro/pull/42/files',
      'https://github.com/kantord/enwiro/pull/42?diff=split',
      'https://github.com/kantord/enwiro/pull/42/files#diff-abc123',
    ]) {
      expect(router.match(url)?.recipe).toBe('enwiro#42')
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

  it('rejects a derived name that fails the rule name claim', () => {
    // Claim caps the number at 19 digits; the URL pattern does not.
    const router = new Router([githubRule])
    expect(
      router.match(`https://github.com/kantord/enwiro/pull/${'9'.repeat(25)}`),
    ).toBeNull()
  })

  it('first match wins across rules in served order', () => {
    const catchAll: RuleEntry = {
      cookbook: 'other',
      namePattern: '^(?:web-(?P<number>[0-9]+))$',
      urlPattern: 'https://github.com/kantord/enwiro/pull/:number([0-9]+)',
      recipeTemplate: 'web-{number}',
    }
    expect(
      new Router([githubRule, catchAll]).match(
        'https://github.com/kantord/enwiro/pull/1',
      )?.cookbook,
    ).toBe('github')
    expect(
      new Router([catchAll, githubRule]).match(
        'https://github.com/kantord/enwiro/pull/1',
      )?.cookbook,
    ).toBe('other')
  })

  it('skips invalid rules without breaking valid ones', () => {
    const broken: RuleEntry = {
      cookbook: 'broken',
      namePattern: '^(?:x)$',
      urlPattern: 'https://github.com/:kind(pull',
      recipeTemplate: 'x',
    }
    const router = new Router([broken, githubRule])
    expect(
      router.match('https://github.com/kantord/enwiro/pull/7')?.recipe,
    ).toBe('enwiro#7')
  })

  it('routes non-literal-host rules through the fallback bucket', () => {
    const anySubdomain: RuleEntry = {
      cookbook: 'forge',
      namePattern: '^(?:t-(?P<number>[0-9]+))$',
      urlPattern: 'https://*.example.com/tickets/:number([0-9]+)',
      recipeTemplate: 't-{number}',
    }
    const router = new Router([anySubdomain])
    expect(router.match('https://tickets.example.com/tickets/9')?.recipe).toBe(
      't-9',
    )
    expect(router.match('https://a.b.example.com/tickets/12')?.recipe).toBe(
      't-12',
    )
    expect(router.match('https://example.org/tickets/9')).toBeNull()
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

describe('rustRegexToJs', () => {
  it('converts Rust named groups to JS syntax', () => {
    const regex = rustRegexToJs('^(?:enwiro#(?P<number>[0-9]{1,19}))$')
    expect(regex.test('enwiro#42')).toBe(true)
    expect(regex.test('enwiro#abc')).toBe(false)
  })

  it('leaves escaped literals alone', () => {
    // recipe_pattern::escape turns a literal `(` into `\(`, so a repo
    // named `x(?P<y` arrives as `x\(\?P<y` and must stay literal.
    const regex = rustRegexToJs('^(?:x\\(\\?P<y#(?P<number>[0-9]+))$')
    expect(regex.test('x(?P<y#5')).toBe(true)
  })
})
