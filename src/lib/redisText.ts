import type { RedisText } from '@/types/ipc'

/**
 * `RedisText` 的读取工具。
 *
 * 这是语义化取值命令的值形状：`text` 与 `base64` 恰好有一个非空。
 * 消费方只在展示层分支（要文本还是要十六进制），不在结构上分支 ——
 * 对照 `redisValue.ts`，那里的 String/Object 双形状正是要摆脱的东西。
 * 见 doc/backend/ipc-contract.md「RedisText」。
 */

/* ── base64 与字节 ───────────────────────────────────── */

export function bytesFromBase64(base64: string): Uint8Array {
  const binary = atob(base64)
  const out = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i)
  return out
}

export function base64FromBytes(bytes: Uint8Array): string {
  let binary = ''
  for (let i = 0; i < bytes.length; i += 1) binary += String.fromCharCode(bytes[i])
  return btoa(binary)
}

export function base64FromText(text: string): string {
  return base64FromBytes(new TextEncoder().encode(text))
}

export function hexFromBase64(base64: string): string {
  const bytes = bytesFromBase64(base64)
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join(' ')
}

/* ── RedisText ───────────────────────────────────────── */

/** 展示用文本。非 UTF-8 的值走十六进制，不要塞一堆 U+FFFD 给用户看。 */
export function textOf(value: RedisText): string {
  if (value.text !== null) return value.text
  if (value.base64 !== null) return hexFromBase64(value.base64)
  return ''
}

/** 原始字节。UTF-8 值由 `text` 还原 —— 后端刻意不多传一份 base64。 */
export function bytesOf(value: RedisText): Uint8Array {
  if (value.base64 !== null) return bytesFromBase64(value.base64)
  return new TextEncoder().encode(value.text ?? '')
}

/** 送回后端时用。codec 与写命令的入参都是 base64 字节。 */
export function base64Of(value: RedisText): string {
  return value.base64 ?? base64FromText(value.text ?? '')
}

/** 非 UTF-8：只能按十六进制看，编辑器要退回只读的字节视图。 */
export function isBinaryText(value: RedisText): boolean {
  return value.text === null && value.base64 !== null
}

/**
 * 被截断过。**此时一律禁止保存** —— 把截断后的值写回去会静默丢掉后面的部分，
 * 这是最难被发现也最不可逆的一类数据损坏。
 */
export function isTruncated(value: RedisText): boolean {
  return value.totalBytes !== null
}

/**
 * 这个值能不能放进文本编辑框、改完原样写回。不能时返回原因，界面照着禁用并说明。
 *
 * 编辑框里放的是 `textOf()` 的产物：截断的值少了后面一段，非 UTF-8 的值是一串
 * 十六进制。照着写回去都是静默的数据损坏。见 doc/backend/ipc-contract.md「截断契约」。
 */
export function textEditBlockedReason(value: RedisText): string | null {
  if (isTruncated(value)) return '值被截断，保存会丢掉未显示的部分'
  if (isBinaryText(value)) return '值不是 UTF-8，编辑框里显示的是十六进制'
  return null
}

/**
 * Set / ZSet 的成员既是值又是定位符：SREM / ZADD / ZREM 都要完整的成员字节，
 * 而截断的成员没有别的定位符可以重取完整字节。拿截断后的字节去写，ZADD 会新增
 * 一个成员，SREM / ZREM 会删到恰好等于这段前缀的另一个成员。
 */
export function memberLocateBlockedReason(member: RedisText): string | null {
  return isTruncated(member) ? '成员被截断，拿不到完整字节就无法定位它，请走 CLI 标签页' : null
}

/** 直接从文本构造，用于写路径。 */
export function textValue(text: string): RedisText {
  return { text, base64: null, totalBytes: null }
}

/* `fatal` 才会在非法序列上抛错；默认模式会静默塞 U+FFFD，那时
   `text` 非空就不再等于"这是合法 UTF-8"，`isBinaryText` 会跟着骗人。 */
const STRICT_UTF8 = new TextDecoder('utf-8', { fatal: true })

/**
 * 字节 → `RedisText`，与后端 `redis_values::text_from_bytes` 同构：
 * 能解成 UTF-8 就给 `text`，否则给 `base64`，恰好一个非空。
 *
 * 前端要这个是因为**反向编码的产物没有过 IPC**：沿链 encode 回去时每一层的字节
 * 都是本地算出来的，而链上存的是 `RedisText`。缺了它只能一律填 `base64`，
 * 于是保存后回退一步，一份 JSON 会被 `isBinaryText` 判成二进制、显示成十六进制。
 */
export function textFromBytes(bytes: Uint8Array): RedisText {
  try {
    return { text: STRICT_UTF8.decode(bytes), base64: null, totalBytes: null }
  } catch {
    return { text: null, base64: base64FromBytes(bytes), totalBytes: null }
  }
}

export const EMPTY_TEXT: RedisText = { text: '', base64: null, totalBytes: null }
