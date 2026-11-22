import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type {
  CapabilityChangeEvent,
  CommandLogEvent,
  ConnectionStatusEvent,
  TaskProgressEvent,
} from '@/types/ipc'

/**
 * 后端发的全部频道。频道名是字符串常量，写错不会报错只会静默收不到，
 * 所以集中在这里定义，别的地方不要出现字面量。
 */
export const CHANNEL = {
  connectionStatus: 'redis://connection-status',
  taskProgress: 'redis://task-progress',
  commandLog: 'redis://command-log',
  capabilityChange: 'redis://capability-change',
} as const

/**
 * 任务进度是唯一的高频频道：Monitor 每秒可能上千条。
 * 订阅方必须自己做节流或缓冲，不要在回调里直接触发重渲染。
 */
export function onTaskProgress(
  handler: (event: TaskProgressEvent) => void,
): Promise<UnlistenFn> {
  return listen<TaskProgressEvent>(CHANNEL.taskProgress, (e) => handler(e.payload))
}

export function onConnectionStatus(
  handler: (event: ConnectionStatusEvent) => void,
): Promise<UnlistenFn> {
  return listen<ConnectionStatusEvent>(CHANNEL.connectionStatus, (e) => handler(e.payload))
}

export function onCommandLog(handler: (event: CommandLogEvent) => void): Promise<UnlistenFn> {
  return listen<CommandLogEvent>(CHANNEL.commandLog, (e) => handler(e.payload))
}

/**
 * 会话能力被运行时降级：受保护的命令被服务端以「不认识」拒绝，说明建连时的判断错了。
 * 见 doc/backend/capabilities.md「运行时降级」。
 */
export function onCapabilityChange(
  handler: (event: CapabilityChangeEvent) => void,
): Promise<UnlistenFn> {
  return listen<CapabilityChangeEvent>(CHANNEL.capabilityChange, (e) => handler(e.payload))
}

export type { UnlistenFn }
