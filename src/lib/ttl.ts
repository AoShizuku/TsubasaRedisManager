import { computed, onBeforeUnmount, ref, watch, type Ref } from 'vue'
import { toDateTimeLocalValue } from './format'

/**
 * TTL 本地倒计时。
 *
 * 不轮询服务端：每秒对每个打开的 key 发一次 TTL，开十个标签页就是 10 rps 的
 * 无谓流量，跨时区 / 时钟漂移时反而更不准。
 *
 * 用 performance.now() 这种单调时钟而不是 Date.now()，
 * 系统时间被调整时倒计时才不会跳变。
 */
export function useTtlCountdown(source: Ref<number | null>) {
  const base = ref<number | null>(null)
  const stampedAt = ref(0)
  let timer: number | undefined

  function stop() {
    window.clearInterval(timer)
    timer = undefined
  }

  watch(
    source,
    (value) => {
      base.value = value
      stampedAt.value = performance.now()
      stop()
      // 只有真在倒数的 key 才需要每秒重绘；-1 / -2 是静态的
      if (value !== null && value > 0) {
        timer = window.setInterval(() => {
          tick.value = performance.now()
        }, 1000)
      }
    },
    { immediate: true },
  )

  const tick = ref(performance.now())
  onBeforeUnmount(stop)

  /** 当前剩余秒数。-1 永不过期、-2 key 不存在，两者原样透传。 */
  const remaining = computed<number | null>(() => {
    if (base.value === null) return null
    if (base.value < 0) return base.value
    const elapsed = Math.floor((tick.value - stampedAt.value) / 1000)
    return Math.max(0, base.value - elapsed)
  })

  return { remaining }
}

export type TtlMode = 'persist' | 'relative' | 'absolute'

/**
 * 条件 flag（能力 expireConditions）。单 key 编辑用不上，批量必须有 ——
 * 见 doc/frontend/batch.md「条件 flag」。
 *
 * `xx` 声明了但界面上没放：它的语义（只改已经有 TTL 的）在批量场景里
 * 没有明确的用途，而多一个选项就多一次误选。要用再放。
 */
export type TtlCondition = 'nx' | 'xx' | 'gt' | 'lt'

export const TTL_CONDITIONS = [
  { value: 'nx', flag: 'NX', label: '仅当前没有过期时间时设置' },
  { value: 'gt', flag: 'GT', label: '仅当会延长现有过期时间时设置' },
  { value: 'lt', flag: 'LT', label: '仅当会缩短现有过期时间时设置' },
] as const

export const TTL_UNITS = [
  { value: 1, label: '秒' },
  { value: 60, label: '分' },
  { value: 3600, label: '时' },
  { value: 86400, label: '天' },
] as const

export const TTL_PRESETS = [
  { seconds: 300, label: '5分' },
  { seconds: 3600, label: '1时' },
  { seconds: 86400, label: '1天' },
  { seconds: 604800, label: '7天' },
  { seconds: 2592000, label: '30天' },
] as const

/**
 * 三个模式直接对应三条命令。
 *
 * `preview` 是要**显示**给用户的那条命令 —— 这个工具的调性应该是
 * "把 Redis 的真相摊给你看"，而不是包一层黑箱。但真正执行的是
 * `redis_set_key_ttl`：命令由后端拼，前端只给模式和数值。
 * 显示真相不等于由前端来发命令，这两件事在 v0.1 被混为一谈了。
 */
export function ttlCommandFor(
  mode: TtlMode,
  key: string,
  seconds: number,
  absoluteMs: number,
  condition?: TtlCondition | null,
): { value: number | null; preview: string } {
  // PERSIST 没有条件 flag，所以后缀只拼给另外两个模式
  const flag = condition
    ? ` ${TTL_CONDITIONS.find((item) => item.value === condition)?.flag ?? condition.toUpperCase()}`
    : ''
  switch (mode) {
    case 'persist':
      return { value: null, preview: `PERSIST ${key}` }
    case 'relative':
      return { value: seconds, preview: `EXPIRE ${key} ${seconds}${flag}` }
    case 'absolute': {
      const unix = Math.floor(absoluteMs / 1000)
      return { value: unix, preview: `EXPIREAT ${key} ${unix}${flag}` }
    }
  }
}

/**
 * 编辑中的 TTL。
 *
 * 单位与数值分开存（而不是只存换算好的秒数）是因为界面要把用户填的东西
 * 原样显示回去 —— 填 `1 天` 不该在重绘后变成 `86400 秒`。
 */
export interface TtlDraft {
  mode: TtlMode
  amount: number
  unit: number
  /** datetime-local 的值 */
  absolute: string
}

/**
 * 按当前实际 TTL 建一份草稿。
 *
 * `current` 是该 key 的剩余秒数（-1 永不过期、null 未知）；批量场景没有
 * 「当前值」这回事，传 null 即可。
 */
export function newTtlDraft(current: number | null): TtlDraft {
  const live = current !== null && current >= 0
  return {
    mode: live ? 'relative' : 'persist',
    amount: live ? current : 86400,
    unit: 1,
    absolute: toDateTimeLocalValue(new Date(Date.now() + Math.max(current ?? 0, 60) * 1000)),
  }
}

/**
 * 从草稿算出要发给后端的东西，以及界面要显示的换算结果。
 *
 * `TtlChip`（单 key popover）与 `BatchTtlDialog`（批量 modal）共用这一份。
 * 两边各算一遍的话，「秒还是毫秒、从现在算还是从纪元算」这类错误迟早
 * 只在其中一边被修掉。
 */
export function useTtlDraft(draft: Ref<TtlDraft>) {
  const seconds = computed(() => Math.max(1, Math.floor(draft.value.amount * draft.value.unit)))
  const absoluteMs = computed(() => new Date(draft.value.absolute).getTime())

  /**
   * 无论用哪种输入方式，都要能给出换算后的绝对时刻。
   * TTL 最常见的翻车是单位和基准搞混 —— 给一个人类可读的结果时刻，
   * 这类错误直接归零。见 doc/frontend/ttl.md「底部永远显示换算出的绝对时刻」。
   */
  const resultAt = computed<Date | null>(() => {
    if (draft.value.mode === 'persist') return null
    const ms = draft.value.mode === 'relative' ? Date.now() + seconds.value * 1000 : absoluteMs.value
    return Number.isFinite(ms) ? new Date(ms) : null
  })

  /** 过去的绝对时刻 = 立即删除。不能让它悄悄通过。 */
  const invalid = computed(
    () =>
      draft.value.mode === 'absolute' &&
      (!Number.isFinite(absoluteMs.value) || absoluteMs.value <= Date.now()),
  )

  /** 发给后端的 value：相对是秒数、绝对是 unix 时间戳、不过期是 null。 */
  const value = computed(
    () => ttlCommandFor(draft.value.mode, '', seconds.value, absoluteMs.value).value,
  )

  return { seconds, absoluteMs, resultAt, invalid, value }
}
