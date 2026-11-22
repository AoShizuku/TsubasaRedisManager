import { describe, expect, it } from 'vitest'
import { memberLocateBlockedReason, textEditBlockedReason } from './redisText'
import type { RedisText } from '@/types/ipc'

const plain: RedisText = { text: 'hello', base64: null, totalBytes: null }
const binary: RedisText = { text: null, base64: 'H4sIAA==', totalBytes: null }
const truncatedText: RedisText = { text: 'a'.repeat(16), base64: null, totalBytes: 70_000 }
const truncatedBinary: RedisText = { text: null, base64: 'H4sIAA==', totalBytes: 70_000 }

describe('textEditBlockedReason', () => {
  it('lets complete UTF-8 values through', () => {
    expect(textEditBlockedReason(plain)).toBeNull()
  })

  it('blocks truncated values, since saving drops the unshown tail', () => {
    expect(textEditBlockedReason(truncatedText)).toMatch('截断')
  })

  it('blocks non-UTF-8 values, since the editor shows hex rather than the bytes', () => {
    expect(textEditBlockedReason(binary)).toMatch('UTF-8')
  })

  it('reports truncation first when a value is both truncated and binary', () => {
    expect(textEditBlockedReason(truncatedBinary)).toMatch('截断')
  })
})

describe('memberLocateBlockedReason', () => {
  it('lets complete members through, binary ones included', () => {
    // 非 UTF-8 的完整成员仍然能用原始字节精确定位，删除与改分数都安全
    expect(memberLocateBlockedReason(plain)).toBeNull()
    expect(memberLocateBlockedReason(binary)).toBeNull()
  })

  it('blocks truncated members, which have no other locator to recover the full bytes', () => {
    expect(memberLocateBlockedReason(truncatedText)).toMatch('截断')
    expect(memberLocateBlockedReason(truncatedBinary)).toMatch('截断')
  })
})
