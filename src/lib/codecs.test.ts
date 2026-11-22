import { describe, expect, it } from 'vitest'
import { CODECS, CONCLUSION_LABEL, DETECT_CANDIDATES, conclusionOf } from './codecs'
import { CODEC_NAMES } from '@/types/ipc'

describe('能力表', () => {
  it('九种都在，与 Rust 的 codec_support_matrix() 对齐', () => {
    expect(CODECS.map((codec) => codec.name).sort()).toEqual([...CODEC_NAMES].sort())
  })

  it('归属查得到', () => {
    expect(conclusionOf('gzip')).toBe('rustRoundTrip')
    expect(conclusionOf('phpSerialize')).toBe('rendererRoundTrip')
    expect(conclusionOf('pickle')).toBe('readonly')
  })

  it('文案不泄漏归属 —— 用户不关心是哪一侧解的', () => {
    expect(CONCLUSION_LABEL.rendererRoundTrip).toBe(CONCLUSION_LABEL.rustRoundTrip)
    for (const label of Object.values(CONCLUSION_LABEL)) {
      expect(label).not.toContain('渲染进程')
    }
  })
})

describe('自动识别的候选集', () => {
  /* doc/frontend/codecs.md「识别的三个维度」：无魔数的格式进了候选就会把一切都识别成它。
     谁想"补齐"这一格，先看那一节。 */
  it('msgpack 与 protobuf 永远进不去', () => {
    expect(DETECT_CANDIDATES).not.toContain('msgpack')
    expect(DETECT_CANDIDATES).not.toContain('protobuf')
  })

  it('phpSerialize 可以进 —— 它的前缀足够可靠', () => {
    expect(DETECT_CANDIDATES).toContain('phpSerialize')
  })

  it('Java 序列化可以进 —— ac ed 00 05 是完整 stream header', () => {
    expect(DETECT_CANDIDATES).toContain('javaSerialize')
  })

  it('跟着 detectable 走，不跟着"能不能跑"走', () => {
    // 候选集只是 detectable 的投影。写死成"已实现的那些"看起来一样，
    // 但 msgpack 实现的那天就会自己长进来 —— 上面两条钉的正是这个差别
    expect(DETECT_CANDIDATES).toEqual(
      CODECS.filter((codec) => codec.detectable).map((codec) => codec.name),
    )
  })
})
