import { describe, expect, it } from 'vitest'
import { killWordBefore } from './lineEdit'

describe('killWordBefore', () => {
  it('删掉光标前的一个词', () => {
    expect(killWordBefore('GET foo', 7)).toEqual({ text: 'GET ', caret: 4 })
  })

  it('先吃掉光标前的空白，再吃掉一个词', () => {
    expect(killWordBefore('GET   foo   ', 12)).toEqual({ text: 'GET   ', caret: 6 })
  })

  it('光标后面的内容原样留着', () => {
    expect(killWordBefore('SET k v', 5)).toEqual({ text: 'SET  v', caret: 4 })
  })

  it('只剩一个词时清空', () => {
    expect(killWordBefore('GET', 3)).toEqual({ text: '', caret: 0 })
  })

  it('空串不炸', () => {
    expect(killWordBefore('', 0)).toEqual({ text: '', caret: 0 })
  })

  // 不判选区的话 head 会被再削掉一个词，"SET k v" 变成空串
  it('有选区时只吃掉选区，不多删前面那个词', () => {
    expect(killWordBefore('SET k v', 4, 7)).toEqual({ text: 'SET ', caret: 4 })
  })
})
