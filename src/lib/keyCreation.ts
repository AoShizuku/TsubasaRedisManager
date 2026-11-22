import { base64FromBytes, base64FromText, bytesFromBase64 } from './redisText'
import type { CreateKeyValue, KeyExpiry } from '@/types/ipc'
import type { TtlDraft } from './ttl'

export interface ByteDraft { text: string; encoding: 'text' | 'base64' }
export interface InitialRow { id: number; field: ByteDraft; value: ByteDraft; score: string }
export const CREATE_TYPES = ['string', 'hash', 'list', 'set', 'zset', 'stream'] as const
export type CreateType = typeof CREATE_TYPES[number]
export const newByteDraft = (): ByteDraft => ({ text: '', encoding: 'text' })

export function encodeDraft(draft: ByteDraft): string {
  if (draft.encoding === 'text') return base64FromText(draft.text)
  const encoded = draft.text.replace(/\s/g, '')
  try {
    return base64FromBytes(bytesFromBase64(encoded))
  } catch {
    throw new Error('Base64 内容无效，请检查输入')
  }
}

export function initialValue(type: CreateType, string: ByteDraft, rows: InitialRow[]): CreateKeyValue {
  if (type === 'string') return { type, valueBase64: encodeDraft(string) }
  if (rows.length === 0 || rows.length > 256) throw new Error('初始内容需要 1 至 256 行')
  if (type === 'hash' || type === 'stream') {
    const fields = rows.map(row => ({ fieldBase64: encodeDraft(row.field), valueBase64: encodeDraft(row.value) }))
    if (type === 'hash' && new Set(fields.map(field => field.fieldBase64)).size !== fields.length) {
      throw new Error('字段重复，请保留每个字段的一份初始值')
    }
    return { type, fields }
  }
  const members = rows.map(row => encodeDraft(row.value))
  if (type !== 'list' && new Set(members).size !== members.length) throw new Error('成员重复，请检查初始内容')
  if (type === 'list') return { type, elementsBase64: members }
  if (type === 'set') return { type, membersBase64: members }
  return {
    type,
    members: rows.map((row, index) => {
      const score = row.score.trim()
      if (!/^[+-]?inf$/i.test(score) && (score === '' || !Number.isFinite(Number(score)))) {
        throw new Error(`第 ${index + 1} 行的 score 无效`)
      }
      return { valueBase64: members[index], score }
    }),
  }
}

export function keyExpiry(draft: TtlDraft, preserve: boolean): KeyExpiry {
  if (preserve) return { mode: 'preserve' }
  if (draft.mode === 'persist') return { mode: 'persist' }
  const milliseconds = draft.mode === 'relative'
    ? Number(draft.amount) * draft.unit * 1000
    : new Date(draft.absolute).getTime()
  if (!Number.isSafeInteger(milliseconds) || milliseconds <= 0 || (draft.mode === 'absolute' && milliseconds <= Date.now())) {
    throw new Error('请输入有效的过期时间，相对时间必须大于零')
  }
  return { mode: draft.mode, milliseconds }
}
