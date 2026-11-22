import type { RedisValue, RedisValueObject } from '@/types/ipc'
import { hexFromBase64 } from '@/lib/redisText'

/**
 * `RedisValue` 的读取工具 —— **CLI 专用**。
 *
 * CLI 里用户手输任意命令，回复形状无法预知，所以这套"什么都能读一点"的
 * 工具在那里是必要的。功能路径不要再用它：语义化取值命令返回的是
 * `RedisText`（见 `redisText.ts`），形状唯一、不需要猜。
 *
 * 同一条命令在 RESP2 与 RESP3 下形状不同，这层负责把差异吃掉：
 *   HGETALL  → RESP2 扁平数组 [f,v,f,v]；RESP3 { type:'map', entries:[…] }
 *   ZRANGE … WITHSCORES → RESP2 扁平数组；RESP3 成对数组
 * 这类协议差异本该由后端吸收，v0.2 正在把它搬回 Rust。
 */

export { bytesFromBase64, base64FromBytes, base64FromText } from '@/lib/redisText'

function isObjectValue(value: RedisValue): value is RedisValueObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function isBinary(value: RedisValue): value is { type: 'binary'; base64: string } {
  return isObjectValue(value) && value.type === 'binary'
}

export function isNil(value: RedisValue): boolean {
  return value === null || value === undefined
}

export function isServerError(
  value: RedisValue,
): value is { type: 'serverError'; value: string } {
  return isObjectValue(value) && value.type === 'serverError'
}

/** 把值渲染成一行文本。二进制走十六进制，容器走 JSON。 */
export function asText(value: RedisValue): string {
  if (value === null || value === undefined) return ''
  if (typeof value === 'string') return value
  if (typeof value === 'number') return String(value)
  if (Array.isArray(value)) return JSON.stringify(value)
  switch (value.type) {
    case 'binary':
      return hexFromBase64(value.base64)
    case 'double':
    case 'bigNumber':
      return String(value.value)
    case 'boolean':
      return value.value ? 'true' : 'false'
    case 'verbatimString':
      return value.value
    case 'serverError':
    case 'unsupported':
      return value.value
    default:
      return JSON.stringify(value)
  }
}

export function asNumber(value: RedisValue): number | null {
  if (typeof value === 'number') return value
  if (typeof value === 'string') {
    const parsed = Number(value)
    return Number.isNaN(parsed) ? null : parsed
  }
  if (isObjectValue(value) && (value.type === 'double' || value.type === 'bigNumber')) {
    const parsed = Number(value.value)
    return Number.isNaN(parsed) ? null : parsed
  }
  return null
}

export function asArray(value: RedisValue): RedisValue[] {
  if (Array.isArray(value)) return value
  if (isNil(value)) return []
  return [value]
}

export function asStrings(value: RedisValue): string[] {
  return asArray(value).map(asText)
}

/**
 * 规整成键值对。同时接受：
 *   - 扁平数组 [k, v, k, v]（RESP2）
 *   - map 对象（RESP3）
 *   - 成对数组 [[k, v], [k, v]]
 * 奇数长度的扁平数组会丢掉最后一个残项，而不是配一个 undefined 出去。
 */
export function asPairs(value: RedisValue): Array<[RedisValue, RedisValue]> {
  if (isObjectValue(value) && value.type === 'map') {
    return value.entries.map((entry) => [entry.key, entry.value])
  }
  const items = asArray(value)
  if (items.length === 0) return []

  const allPairs = items.every((item) => Array.isArray(item) && item.length === 2)
  if (allPairs) {
    return (items as RedisValue[][]).map((pair) => [pair[0], pair[1]])
  }

  const pairs: Array<[RedisValue, RedisValue]> = []
  for (let i = 0; i + 1 < items.length; i += 2) {
    pairs.push([items[i], items[i + 1]])
  }
  return pairs
}

/**
 * SCAN 家族（HSCAN / SSCAN / ZSCAN）的返回：[cursor, elements]。
 * cursor 在 RESP2 下是字符串，RESP3 下可能是整数。
 */
export function asScanReply(value: RedisValue): { cursor: string; items: RedisValue[] } {
  const parts = asArray(value)
  if (parts.length < 2) return { cursor: '0', items: [] }
  return { cursor: asText(parts[0]) || '0', items: asArray(parts[1]) }
}

/* ── 值的形态识别 ─────────────────────────────────────── */

export type ValueShape = 'json' | 'number' | 'binary' | 'text'

/**
 * 给值预览挑一个标注（那个 .vtag 小标签）。
 * 只做廉价判断：JSON 检测会真的 parse，所以超过阈值就不试了。
 */
export function detectShape(value: RedisValue, jsonProbeLimit = 64 * 1024): ValueShape {
  if (isBinary(value)) return 'binary'
  if (typeof value === 'number') return 'number'
  if (typeof value !== 'string') return 'text'

  const trimmed = value.trim()
  if (trimmed.length > jsonProbeLimit) return 'text'
  if (trimmed.length >= 2) {
    const head = trimmed[0]
    const tail = trimmed[trimmed.length - 1]
    if ((head === '{' && tail === '}') || (head === '[' && tail === ']')) {
      try {
        JSON.parse(trimmed)
        return 'json'
      } catch {
        return 'text'
      }
    }
  }
  if (trimmed !== '' && !Number.isNaN(Number(trimmed))) return 'number'
  return 'text'
}

export function prettyJson(text: string): string | null {
  try {
    return JSON.stringify(JSON.parse(text), null, 2)
  } catch {
    return null
  }
}
