import { describe, expect, it } from 'vitest'
import { encodeDraft, initialValue, keyExpiry, newByteDraft, type InitialRow } from './keyCreation'

const row = (text: string, encoding: 'text' | 'base64' = 'text'): InitialRow => ({
  id: 0, field: { text, encoding }, value: { text, encoding }, score: '0',
})

describe('initial key content', () => {
  it('preserves empty text, whitespace, newlines and binary bytes', () => {
    expect(encodeDraft(newByteDraft())).toBe('')
    expect(encodeDraft({ text: ' a\n ', encoding: 'text' })).toBe('IGEKIA==')
    expect(encodeDraft({ text: 'AP+A\r\n', encoding: 'base64' })).toBe('AP+A')
    expect(() => encodeDraft({ text: '**invalid**', encoding: 'base64' })).toThrow('Base64')
  })
  it('detects duplicates by bytes across input formats without rejecting repeated list items', () => {
    const rows = [row('a'), row('YQ==', 'base64')]
    for (const type of ['hash', 'set', 'zset'] as const) {
      expect(() => initialValue(type, newByteDraft(), rows)).toThrow('重复')
    }
    expect(initialValue('list', newByteDraft(), rows)).toEqual({ type: 'list', elementsBase64: ['YQ==', 'YQ=='] })
  })
  it('rejects empty collections and bad scores while permitting Redis infinities', () => {
    expect(() => initialValue('list', newByteDraft(), [])).toThrow()
    for (const score of ['', 'NaN', 'oops']) {
      expect(() => initialValue('zset', newByteDraft(), [{ ...row('a'), score }])).toThrow('score')
    }
    expect(initialValue('zset', newByteDraft(), [{ ...row('a'), score: '-inf' }])).toMatchObject({ members: [{ score: '-inf' }] })
  })
})

describe('creation expiration', () => {
  const draft = { mode: 'relative' as const, amount: 1, unit: 60, absolute: '' }
  it('uses milliseconds and keeps preserve distinct from persist', () => {
    expect(keyExpiry(draft, false)).toEqual({ mode: 'relative', milliseconds: 60000 })
    expect(keyExpiry(draft, true)).toEqual({ mode: 'preserve' })
    expect(keyExpiry({ ...draft, mode: 'persist' }, false)).toEqual({ mode: 'persist' })
  })
  it('does not silently clamp an invalid TTL to one second', () => {
    for (const amount of [0, -1, NaN, Infinity]) expect(() => keyExpiry({ ...draft, amount }, false)).toThrow()
    expect(() => keyExpiry({ ...draft, mode: 'absolute', absolute: '2000-01-01T00:00' }, false)).toThrow()
  })
})
