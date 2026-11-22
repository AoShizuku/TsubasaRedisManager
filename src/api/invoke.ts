import { invoke } from '@tauri-apps/api/core'
import type { AppErrorPayload } from '@/types/ipc'

/**
 * 后端 AppError 序列化后就是 AppErrorPayload，Tauri 把它作为 reject 值抛出来。
 * 直接 catch 到的是个裸对象，不是 Error —— 既没有栈也不能被 instanceof 判别，
 * 所以这里统一包成 AppFault，让上层可以按 code 分支处理。
 */
export class AppFault extends Error {
  readonly code: string
  readonly detail: string | null
  readonly recoverable: boolean

  constructor(payload: AppErrorPayload) {
    super(payload.message)
    this.name = 'AppFault'
    this.code = payload.code
    this.detail = payload.detail
    this.recoverable = payload.recoverable
  }

  /** 面向用户的一行说明：有 detail 时把它接在后面，否则只给 message。 */
  get text(): string {
    return this.detail ? `${this.message}：${this.detail}` : this.message
  }
}

function isErrorPayload(value: unknown): value is AppErrorPayload {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as AppErrorPayload).code === 'string' &&
    typeof (value as AppErrorPayload).message === 'string'
  )
}

/** 把任意 catch 到的东西转成 AppFault，调用点因此永远只需处理一种错误类型。 */
export function toFault(error: unknown): AppFault {
  if (error instanceof AppFault) return error
  if (isErrorPayload(error)) return new AppFault(error)
  if (error instanceof Error) {
    return new AppFault({
      code: 'frontend.unexpected',
      message: error.message,
      detail: null,
      recoverable: true,
    })
  }
  return new AppFault({
    code: 'frontend.unexpected',
    message: String(error),
    detail: null,
    recoverable: true,
  })
}

/** 所有 Tauri 调用的唯一入口。不要在别处直接 import invoke。 */
export async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (error) {
    throw toFault(error)
  }
}

/** 只读连接拒绝写命令时的 code，界面上要区别于真正的失败。 */
export const READONLY_REJECTED = 'redis.readonly_write_rejected'
/** 会话已经不存在（连接被关掉、后端重启）。上层应当清理该连接的所有标签页。 */
export const CONNECTION_GONE = 'redis.connection_not_found'
