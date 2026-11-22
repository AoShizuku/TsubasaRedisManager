import type {
  ConnectionEditDraft,
  ConnectionSaveDraft,
  ConnectionStoreSentinelDraft,
  ConnectionStoreSshDraft,
  SslOptions,
} from '@/types/ipc'

/** 连接对话框的表单模型。比 ConnectionSaveDraft 多几个开关位，提交时再收敛回去。 */
export interface ConnectionForm {
  key: string | null
  name: string
  host: string
  port: number
  username: string
  auth: string
  /** 密钥链里已经存了密码；此时 auth 留空表示"保持不变" */
  hasAuth: boolean
  clearAuth: boolean
  db: number
  separator: string
  readonly: boolean
  cluster: boolean
  color: string

  tlsEnabled: boolean
  tls: SslOptions

  sshEnabled: boolean
  ssh: ConnectionStoreSshDraft
  hasSshPassword: boolean
  hasSshPassphrase: boolean

  sentinelEnabled: boolean
  sentinel: ConnectionStoreSentinelDraft
  hasSentinelNodePassword: boolean
}

export const CONNECTION_COLORS = [
  '',
  '#d6372b',
  '#b45309',
  '#15803d',
  '#0e7490',
  '#2563eb',
  '#6d28d9',
  '#be185d',
]

export function emptyForm(): ConnectionForm {
  return {
    key: null,
    name: '',
    host: '127.0.0.1',
    port: 6379,
    username: '',
    auth: '',
    hasAuth: false,
    clearAuth: false,
    db: 0,
    separator: ':',
    readonly: false,
    cluster: false,
    color: '',

    tlsEnabled: false,
    tls: { key: null, cert: null, ca: null, servername: null, insecure: false },

    sshEnabled: false,
    ssh: { host: '', port: 22, username: '', password: '', privatekey: '', passphrase: '', timeout: 30 },
    hasSshPassword: false,
    hasSshPassphrase: false,

    sentinelEnabled: false,
    sentinel: { masterName: '', nodePassword: '' },
    hasSentinelNodePassword: false,
  }
}

export function formFromDraft(draft: ConnectionEditDraft): ConnectionForm {
  const base = emptyForm()
  return {
    ...base,
    key: draft.key,
    name: draft.name ?? '',
    host: draft.host,
    port: draft.port,
    username: draft.username ?? '',
    auth: '',
    hasAuth: draft.hasAuth,
    db: draft.db ?? 0,
    separator: draft.separator ?? ':',
    readonly: draft.connectionReadOnly ?? false,
    cluster: draft.cluster ?? false,
    color: draft.color ?? '',

    tlsEnabled: !!draft.sslOptions,
    tls: draft.sslOptions ?? base.tls,

    sshEnabled: !!draft.sshOptions,
    ssh: draft.sshOptions
      ? { ...draft.sshOptions, password: '', passphrase: '' }
      : base.ssh,
    hasSshPassword: draft.hasSshPassword,
    hasSshPassphrase: draft.hasSshPassphrase,

    sentinelEnabled: !!draft.sentinelOptions,
    sentinel: draft.sentinelOptions
      ? { ...draft.sentinelOptions, nodePassword: '' }
      : base.sentinel,
    hasSentinelNodePassword: draft.hasSentinelNodePassword,
  }
}

/** 空字符串一律转 null：后端把 "" 和 null 当两回事，前者会被写成一个空秘密。 */
function nullable(value: string | null | undefined): string | null {
  const trimmed = (value ?? '').trim()
  return trimmed ? trimmed : null
}

export function draftFromForm(form: ConnectionForm): ConnectionSaveDraft {
  return {
    key: form.key,
    name: nullable(form.name),
    host: form.host.trim(),
    port: form.port,
    username: nullable(form.username),
    db: form.db,
    separator: form.separator,
    connectionReadOnly: form.readonly,
    cluster: form.cluster,
    color: nullable(form.color),
    // 秘密字段的三态：留空 = 保持不变，有值 = 覆写，clear* = 从密钥链删除
    auth: nullable(form.auth),
    clearAuth: form.clearAuth ? true : null,

    sslOptions: form.tlsEnabled
      ? {
          key: nullable(form.tls.key),
          cert: nullable(form.tls.cert),
          ca: nullable(form.tls.ca),
          servername: nullable(form.tls.servername),
          insecure: form.tls.insecure ?? false,
        }
      : null,

    sshOptions: form.sshEnabled
      ? {
          host: form.ssh.host.trim(),
          port: form.ssh.port,
          username: form.ssh.username.trim(),
          password: nullable(form.ssh.password),
          privatekey: nullable(form.ssh.privatekey),
          passphrase: nullable(form.ssh.passphrase),
          timeout: form.ssh.timeout ?? 30,
        }
      : null,

    sentinelOptions: form.sentinelEnabled
      ? {
          masterName: form.sentinel.masterName.trim(),
          nodePassword: nullable(form.sentinel.nodePassword),
        }
      : null,
  }
}

export interface ValidationIssue {
  tab: 'general' | 'tls' | 'ssh' | 'sentinel' | 'advanced'
  message: string
}

/**
 * 与 services/config.rs 的 build_connection_plan 保持同一套规则。
 * 客户端先挡一遍只是为了即时反馈，后端那道校验才是权威。
 */
export function validate(form: ConnectionForm): ValidationIssue[] {
  const issues: ValidationIssue[] = []

  if (!form.host.trim()) {
    issues.push({ tab: 'general', message: '必须填写 Redis 主机地址' })
  }
  if (form.cluster && form.sentinelEnabled) {
    issues.push({ tab: 'advanced', message: '集群与哨兵不能同时启用' })
  }
  if (form.sentinelEnabled && !form.sentinel.masterName.trim()) {
    issues.push({ tab: 'sentinel', message: '必须填写哨兵的 master name' })
  }
  if (form.sshEnabled) {
    if (!form.ssh.host.trim() || !form.ssh.username.trim()) {
      issues.push({ tab: 'ssh', message: 'SSH 必须填写主机与用户名' })
    }
    const hasSecret =
      !!form.ssh.password?.trim() ||
      !!form.ssh.privatekey?.trim() ||
      form.hasSshPassword
    if (!hasSecret) {
      issues.push({ tab: 'ssh', message: 'SSH 必须提供密码或私钥' })
    }
  }
  return issues
}
