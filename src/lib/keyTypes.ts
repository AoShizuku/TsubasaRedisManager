/**
 * Redis 类型元数据。
 *
 * 类型色只出现在三处：树/列表的圆点、标签页的圆点、Key 元信息头的徽标。
 * 表格内部不上色，否则密集数据会退化成色块墙。
 * 也刻意不给类型配图标 —— 六种类型没有公认图标，硬配只增加认知负担。
 */

export const KEY_TYPES = ['string', 'hash', 'list', 'set', 'zset', 'stream'] as const
export type KeyType = (typeof KEY_TYPES)[number]

/** TYPE 返回 none 表示 key 不存在。 */
export type KeyTypeOrNone = KeyType | 'none'

export interface KeyTypeMeta {
  /** 元信息头的文字徽标 */
  label: string
  /** 集合成员的量词，用在 `9 字段` / `24 成员` 这类计数上 */
  unit: string
  /** 该类型的元素计数命令 */
  sizeCommand: string
}

const META: Record<KeyType, KeyTypeMeta> = {
  string: { label: 'STRING', unit: '字节', sizeCommand: 'STRLEN' },
  hash: { label: 'HASH', unit: '字段', sizeCommand: 'HLEN' },
  list: { label: 'LIST', unit: '元素', sizeCommand: 'LLEN' },
  set: { label: 'SET', unit: '成员', sizeCommand: 'SCARD' },
  zset: { label: 'ZSET', unit: '成员', sizeCommand: 'ZCARD' },
  stream: { label: 'STREAM', unit: '条目', sizeCommand: 'XLEN' },
}

export function isKeyType(value: string | null | undefined): value is KeyType {
  return !!value && (KEY_TYPES as readonly string[]).includes(value)
}

export function keyTypeMeta(type: string | null | undefined): KeyTypeMeta {
  return isKeyType(type) ? META[type] : { label: '未知', unit: '项', sizeCommand: 'TYPE' }
}

/** 色点与徽标共用的 class 后缀；未知类型退回中性灰。 */
export function keyTypeClass(type: string | null | undefined): string {
  return isKeyType(type) ? type : 'none'
}
