/** 展示层格式化。凡是会被渲染成数字的地方都要配 tabular-nums，见 doc/frontend/visual-language.md「数字对齐」。 */

const KIB = 1024

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) return '—'
  if (bytes < KIB) return `${bytes} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let value = bytes / KIB
  let unit = 0
  while (value >= KIB && unit < units.length - 1) {
    value /= KIB
    unit += 1
  }
  // 小于 10 时保留两位，否则保留一位：让宽度稳定在 6～7 个字符
  return `${value < 10 ? value.toFixed(2) : value.toFixed(1)} ${units[unit]}`
}

export function formatCount(value: number): string {
  return value.toLocaleString('en-US')
}

/**
 * SCAN 未完成时计数是部分值，必须带 `+`。
 * ARDM 直接显示部分值，会造成"这个命名空间就 312 个 key"的误判。
 */
export function formatPartialCount(value: number, complete: boolean): string {
  return complete ? formatCount(value) : `${formatCount(value)}+`
}

/**
 * 扫描进度估算的显示文本（doc/frontend/key-tree.md「诚实信号」）。
 *
 * 「约」不是措辞是契约：估算来自 SCAN 游标的反转二进制，rehash 抖动、
 * DBSIZE 脱节都会让它失真 —— 集中在这一个函数里，任何调用点都丢不掉
 * 「约」字。取整用 floor 且封顶 99：任务还在跑，「约 100%」是自相矛盾
 * 的一句话，真正的 100% 由任务完成态自己表达。
 */
export function formatApproxPercent(fraction: number): string {
  return `约 ${Math.min(99, Math.floor(fraction * 100))}%`
}

const PERSIST_TTL = -1
const MISSING_TTL = -2

export type TtlState = 'persist' | 'live' | 'soon' | 'gone'

export function ttlState(seconds: number, soonThreshold = 60): TtlState {
  if (seconds === PERSIST_TTL) return 'persist'
  if (seconds === MISSING_TTL || seconds <= 0) return 'gone'
  return seconds <= soonThreshold ? 'soon' : 'live'
}

/**
 * 倒计时文本。24 小时以内是 `HH:MM:SS`，超过则前置天数。
 * 位宽必须稳定，否则每秒重绘时 chip 会左右跳。
 */
export function formatTtl(seconds: number): string {
  if (seconds === PERSIST_TTL) return '不过期'
  if (seconds === MISSING_TTL) return '已删除'
  if (seconds <= 0) return '已过期'

  const days = Math.floor(seconds / 86400)
  const rest = seconds % 86400
  const hh = String(Math.floor(rest / 3600)).padStart(2, '0')
  const mm = String(Math.floor((rest % 3600) / 60)).padStart(2, '0')
  const ss = String(rest % 60).padStart(2, '0')
  return days > 0 ? `${days}天 ${hh}:${mm}:${ss}` : `${hh}:${mm}:${ss}`
}

const pad = (value: number) => String(value).padStart(2, '0')

/** 本地时区的 `YYYY-MM-DD HH:MM:SS`。TTL popover 底部靠它把相对时间摊成绝对时刻。 */
/**
 * 人类可读的时间跨度，用在「这些 Key 将在 X 内消失」这类句子里。
 *
 * 和 `formatTtl` 的 `00:01:00` 是两回事：那个是读数，等宽对齐、每秒重绘；
 * 这个要念得出来，塞进一句警告里。
 */
export function formatSpan(seconds: number): string {
  const value = Math.max(0, Math.round(seconds))
  if (value < 60) return `${value} 秒`
  if (value < 3600) return `${Math.round(value / 60)} 分钟`
  if (value < 86400) return `${Math.round(value / 3600)} 小时`
  return `${Math.round(value / 86400)} 天`
}

export function formatDateTime(date: Date): string {
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ` +
    `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
  )
}

/** `<input type="datetime-local">` 要的格式，秒会被截掉。 */
export function toDateTimeLocalValue(date: Date): string {
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}` +
    `T${pad(date.getHours())}:${pad(date.getMinutes())}`
  )
}

export function formatElapsed(ms: number): string {
  if (ms < 1000) return `${ms} ms`
  return `${(ms / 1000).toFixed(2)} s`
}

export function formatRelativeTime(timestamp: number, now = Date.now()): string {
  const diff = Math.max(0, Math.floor((now - timestamp) / 1000))
  if (diff < 60) return '刚刚'
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`
  return `${Math.floor(diff / 86400)} 天前`
}

/** 单行预览：换行与制表符会撑破 34px 的行高，先压成可见转义。 */
export function toSingleLine(value: string, limit = 400): string {
  const flat = value.replace(/\r/g, '').replace(/\n/g, '↵ ').replace(/\t/g, '  ')
  return flat.length > limit ? `${flat.slice(0, limit)}…` : flat
}

export function formatHexPreview(bytes: Uint8Array, limit = 24): string {
  const shown = Array.from(bytes.slice(0, limit))
    .map((b) => b.toString(16).padStart(2, '0'))
    .join(' ')
  return bytes.length > limit ? `${shown} …` : shown
}
