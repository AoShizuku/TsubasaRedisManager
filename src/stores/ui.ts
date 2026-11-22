import { defineStore } from 'pinia'
import { ref } from 'vue'
import { toFault } from '@/api/invoke'

export type ToastKind = 'ok' | 'warn' | 'danger' | 'info'

export interface Toast {
  id: number
  kind: ToastKind
  text: string
  detail: string | null
}

export type SettingsSection =
  | 'general'
  | 'appearance'
  | 'editor'
  | 'security'
  | 'viewer'
  | 'formatter'
  | 'shortcuts'
  | 'about'

/**
 * 批量设置 TTL 对话框的目标。
 *
 * 两种作用范围对应两个入口：表格勾选来的显式列表、树里右键 namespace 拿到的
 * 匹配模式。见 doc/frontend/batch.md「两个入口」。
 */
export interface BatchTtlTarget {
  sessionId: string
  /** 勾选来的 key；从树进来时为空 */
  keys: string[]
  /** 预填的匹配模式 */
  pattern: string
  /** 打开时先选中哪一种作用范围 */
  scope: 'keys' | 'pattern'
}

/**
 * 批量删除对话框的目标。和 BatchTtlTarget 几乎同构 —— 两个功能共用同一套
 * 「作用范围」表达（显式列表 / 匹配模式），入口也是同两个。
 *
 * 只多一个 `prefix`：删除是唯一一个删完树上就该少一片的批量操作，
 * 而 TTL 改的 key 还在。
 */
export interface BatchDeleteTarget {
  sessionId: string
  /** 勾选来的 key；从树进来时为空 */
  keys: string[]
  /** 预填的匹配模式 */
  pattern: string
  /** 打开时先选中哪一种作用范围 */
  scope: 'keys' | 'pattern'
  /**
   * 删完要从树里裁掉的**字面前缀**（含尾随分隔符）。只有树入口有，表格入口传 null。
   *
   * 它不是 `pattern` 的副本：`pattern` 已经被 `matchPatternFor` 转义过，
   * 拿它去比字符串会把 `evil\[ab\]:` 当成真实前缀。裁树要的是转义前的字面值。
   */
  prefix: string | null
}

let toastId = 0

export type KeyWriteTarget =
  | { mode: 'create'; sessionId: string; prefix: string }
  | { mode: 'clone'; sessionId: string; key: string; taskId?: string }

/** 对话框、通知这类跨组件的界面状态。 */
export const useUiStore = defineStore('ui', () => {
  const keyWriteDialog = ref<KeyWriteTarget | null>(null)
  function openCreateKey(sessionId: string, prefix = '') {
    keyWriteDialog.value = { mode: 'create', sessionId, prefix }
  }
  function openCloneKey(sessionId: string, key: string, taskId?: string) {
    keyWriteDialog.value = { mode: 'clone', sessionId, key, taskId }
  }
  const toasts = ref<Toast[]>([])

  /** 打开连接对话框：null = 新建，字符串 = 编辑该 key。 */
  const connectionDialog = ref<{ open: boolean; key: string | null }>({ open: false, key: null })
  const settingsDialog = ref<{ open: boolean; section: SettingsSection }>({
    open: false,
    section: 'general',
  })

  function notify(kind: ToastKind, text: string, detail: string | null = null) {
    const id = (toastId += 1)
    toasts.value.push({ id, kind, text, detail })
    // 危险级别的消息不自动消失：用户需要时间读完错误详情
    if (kind !== 'danger') {
      window.setTimeout(() => dismiss(id), 4200)
    }
  }

  /** 统一的错误落地口：把 AppFault 的 code/detail 一并带给用户。 */
  function notifyError(error: unknown, prefix?: string) {
    const fault = toFault(error)
    notify('danger', prefix ? `${prefix}：${fault.message}` : fault.message, fault.detail)
    return fault
  }

  function dismiss(id: number) {
    toasts.value = toasts.value.filter((toast) => toast.id !== id)
  }

  /* 确认框。删除类操作走它，按钮用 Redis 红 —— 红色只留给品牌与危险。 */
  const confirmState = ref<{
    open: boolean
    title: string
    body: string
    detail: string | null
    confirmText: string
    danger: boolean
  } | null>(null)
  let confirmResolve: ((value: boolean) => void) | null = null

  function confirm(options: {
    title: string
    body: string
    detail?: string
    confirmText?: string
    danger?: boolean
  }): Promise<boolean> {
    confirmState.value = {
      open: true,
      title: options.title,
      body: options.body,
      detail: options.detail ?? null,
      confirmText: options.confirmText ?? '确定',
      danger: options.danger ?? false,
    }
    return new Promise<boolean>((resolve) => {
      confirmResolve = resolve
    })
  }

  function resolveConfirm(value: boolean) {
    if (confirmState.value) confirmState.value.open = false
    confirmResolve?.(value)
    confirmResolve = null
  }

  /* 值编辑框。不用 window.prompt：Tauri 的 webview 对它的支持不一致，
     而且原生弹窗和整套视觉完全不搭。 */
  const promptState = ref<{
    open: boolean
    title: string
    fields: Array<{ name: string; label: string; value: string; multiline?: boolean; mono?: boolean }>
    confirmText: string
  } | null>(null)
  let promptResolve: ((value: Record<string, string> | null) => void) | null = null

  function prompt(options: {
    title: string
    fields: Array<{ name: string; label: string; value?: string; multiline?: boolean; mono?: boolean }>
    confirmText?: string
  }): Promise<Record<string, string> | null> {
    promptState.value = {
      open: true,
      title: options.title,
      fields: options.fields.map((field) => ({ ...field, value: field.value ?? '' })),
      confirmText: options.confirmText ?? '保存',
    }
    return new Promise((resolve) => {
      promptResolve = resolve
    })
  }

  function resolvePrompt(value: Record<string, string> | null) {
    if (promptState.value) promptState.value.open = false
    promptResolve?.(value)
    promptResolve = null
  }

  /* 批量设置 TTL。低频、高风险，所以是 modal 而不是单 key 那种 popover ——
     判据见 doc/frontend/batch.md「批量不是 for 循环」。 */
  const batchTtlDialog = ref<BatchTtlTarget | null>(null)

  function openBatchTtl(target: BatchTtlTarget) {
    batchTtlDialog.value = target
  }

  function closeBatchTtl() {
    batchTtlDialog.value = null
  }

  /* 批量删除。同样是 modal，判据同上：低频、高风险、**不可撤销** ——
     删掉的 key 没有任何办法找回来。低频所以弹一个框不烦人，高风险所以
     值得让用户停一下把作用范围看清楚。见 doc/frontend/batch.md「批量不是 for 循环」。 */
  const batchDeleteDialog = ref<BatchDeleteTarget | null>(null)

  function openBatchDelete(target: BatchDeleteTarget) {
    batchDeleteDialog.value = target
  }

  function closeBatchDelete() {
    batchDeleteDialog.value = null
  }

  function openConnectionDialog(key: string | null = null) {
    connectionDialog.value = { open: true, key }
  }

  function closeConnectionDialog() {
    connectionDialog.value = { open: false, key: null }
  }

  function openSettings(section: SettingsSection = 'general') {
    settingsDialog.value = { open: true, section }
  }

  function closeSettings() {
    settingsDialog.value = { ...settingsDialog.value, open: false }
  }

  return {
    keyWriteDialog,
    openCreateKey,
    openCloneKey,
    toasts,
    connectionDialog,
    settingsDialog,
    batchTtlDialog,
    openBatchTtl,
    closeBatchTtl,
    batchDeleteDialog,
    openBatchDelete,
    closeBatchDelete,
    confirmState,
    confirm,
    resolveConfirm,
    promptState,
    prompt,
    resolvePrompt,
    notify,
    notifyError,
    dismiss,
    openConnectionDialog,
    closeConnectionDialog,
    openSettings,
    closeSettings,
  }
})
