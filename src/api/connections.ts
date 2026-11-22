import { call } from './invoke'
import type {
  ConnectionEditDraft,
  ConnectionMigrationResult,
  ConnectionSaveDraft,
  ConnectionSummary,
} from '@/types/ipc'

/** 存储态连接的读写。秘密走系统密钥链，永远不会出现在返回值里。 */

export function listConnections(): Promise<ConnectionSummary[]> {
  return call('connection_store_list')
}

export function getConnection(key: string): Promise<ConnectionEditDraft> {
  return call('connection_store_get', { key })
}

export function saveConnection(draft: ConnectionSaveDraft): Promise<ConnectionSummary> {
  return call('connection_store_save', { draft })
}

export function deleteConnection(key: string): Promise<void> {
  return call('connection_store_delete', { key })
}

export function reorderConnections(keys: string[]): Promise<ConnectionSummary[]> {
  return call('connection_store_reorder', { keys })
}

/** 从 ARDM 的 connections 配置导入。payload 是对方的原始 JSON。 */
export function migrateLegacyConnections(payload: unknown): Promise<ConnectionMigrationResult> {
  return call('connection_store_migrate_legacy', { payload })
}
