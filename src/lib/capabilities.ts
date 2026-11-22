import type { Capabilities, CapabilityId } from '@/types/ipc'

/**
 * 功能被能力挡住时的原因，能用时为 null。
 *
 * 只看 `usable`：unknown 算不算能用由后端注册表的「未知时」策略决定，前端不再判一遍。
 * 原因原样用后端的 `reason`。见 doc/backend/capabilities.md。
 *
 * 会话不在（null）时不挡 —— 那种情况下调用会以 connection_not_found 失败，由那条路径报。
 */
export function capabilityBlockedReason(
  capabilities: Capabilities | null | undefined,
  id: CapabilityId,
): string | null {
  const capability = capabilities?.[id]
  if (!capability || capability.usable) return null
  return capability.reason
}
