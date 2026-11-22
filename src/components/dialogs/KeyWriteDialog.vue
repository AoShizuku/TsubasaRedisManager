<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import Dlg from '@/components/ui/Dlg.vue'
import Btn from '@/components/ui/Btn.vue'
import Cbx from '@/components/ui/Cbx.vue'
import Icon from '@/components/ui/Icon.vue'
import TypeChip from '@/components/ui/TypeChip.vue'
import TtlModePicker from '@/components/keys/TtlModePicker.vue'
import KeyBytesInput from './KeyBytesInput.vue'
import { startCloneKey, createKey, getKeyMeta, keyExists } from '@/api/redis'
import { toFault } from '@/api/invoke'
import { capabilityBlockedReason } from '@/lib/capabilities'
import { CREATE_TYPES, initialValue, keyExpiry, newByteDraft, type CreateType, type InitialRow } from '@/lib/keyCreation'
import { newTtlDraft, useTtlDraft } from '@/lib/ttl'
import { formatDateTime, formatBytes } from '@/lib/format'
import { useConnectionsStore } from '@/stores/connections'
import { useClonesStore, cloneRunning } from '@/stores/clones'
import { useTasksStore } from '@/stores/tasks'
import { useSessionsStore } from '@/stores/sessions'
import { useTabsStore } from '@/stores/tabs'
import { useUiStore } from '@/stores/ui'

/**
 * 新建 / 克隆 Key。
 *
 * 和批量 TTL、批量删除同一个骨架：标题下写作用范围，正文按 .form-sec 分节，
 * 页脚左侧先说后果再放按钮。它们是同一类东西 —— 从模态里发起的写操作。
 * 规则见 doc/frontend/key-write-dialog.md。
 */
const ui = useUiStore()
const sessions = useSessionsStore()
const tabs = useTabsStore()
const connections = useConnectionsStore()
const clones = useClonesStore()
const tasks = useTasksStore()
const target = computed(() => ui.keyWriteDialog)
const cloning = computed(() => target.value?.mode === 'clone')
const source = computed(() => target.value ? sessions.sessions.get(target.value.sessionId) : undefined)
const scopeLabel = computed(() => (source.value ? `${source.value.name} / db${source.value.db}` : ''))
const busy = ref(false)
const open = computed({ get: () => !!target.value, set: value => { if (!value && !busy.value) ui.keyWriteDialog = null } })
const name = ref('')
const nameInput = ref<HTMLInputElement | null>(null)
const db = ref(0)
const connectionKey = ref('')
const taskId = ref<string | null>(null)
const closeOnSuccess = ref<string | null>(null)
const job = computed(() => taskId.value ? clones.jobs.get(taskId.value) : undefined)
const running = computed(() => !!taskId.value && (!job.value || cloneRunning(job.value.phase)))
const selectedConnection = computed(() => connections.byKey.get(connectionKey.value))
const targetCluster = computed(() => destination.value?.cluster ?? selectedConnection.value?.cluster ?? source.value?.cluster ?? false)
const sameConnection = computed(() => connectionKey.value === (source.value?.connectionKey ?? ''))
const type = ref<CreateType>('string')
const string = ref(newByteDraft())
let rowId = 0
const newRow = (): InitialRow => ({ id: rowId++, field: newByteDraft(), value: newByteDraft(), score: '0' })
const rows = ref<InitialRow[]>([newRow()])
const draft = ref(newTtlDraft(null))
const preserve = ref(true)
const openAfter = ref(true)
const error = ref('')
const conflict = ref(false)
const uncertain = ref(false)
const written = ref(false)
const sourceType = ref<string | null>(null)
const submitted = ref<{ sessionId: string; key: string } | null>(null)
const { resultAt } = useTtlDraft(draft)
const destination = computed(() => {
  const origin = source.value
  if (!origin) return undefined
  if (sameConnection.value && db.value === origin.db) return origin
  const id = connectionKey.value ? sessions.sessionIdFor(connectionKey.value, db.value) : null
  return id ? sessions.sessions.get(id) : undefined
})
/**
 * 新建是目标上的一个脚本；克隆的快照在源上、恢复在目标上，两端都是脚本。
 * 目标会话还没开（跨连接、未展开的库）时查不到，提交时由后端的同一条守卫拦。
 */
const scriptingBlocked = computed(() =>
  capabilityBlockedReason(destination.value?.capabilities, 'scripting')
  ?? (cloning.value ? capabilityBlockedReason(source.value?.capabilities, 'scripting') : null),
)
const canSubmit = computed(() => !!source.value && !busy.value && !running.value && !written.value && !uncertain.value && !destination.value?.readonly && !scriptingBlocked.value)
/** hash 与 stream 的一行是「字段 + 值」，其余类型只有一个成员格 */
const hasField = computed(() => type.value === 'hash' || type.value === 'stream')
/** 克隆且勾了「保留到期时刻」时不用自己选 TTL */
const customTtl = computed(() => !cloning.value || !preserve.value)
let restoringForm = false

watch(target, async next => {
  if (!next) return
  restoringForm = true
  taskId.value = next.mode === 'clone' ? next.taskId ?? null : null
  closeOnSuccess.value = null
  connectionKey.value = source.value?.connectionKey ?? ''
  db.value = source.value?.db ?? 0
  name.value = next.mode === 'clone' ? `${next.key}_copy` : next.prefix
  type.value = 'string'
  string.value = newByteDraft()
  rows.value = [newRow()]
  draft.value = newTtlDraft(null)
  preserve.value = true
  openAfter.value = true
  error.value = ''
  conflict.value = false
  uncertain.value = false
  written.value = false
  submitted.value = null
  sourceType.value = null
  if (taskId.value) {
    const previous = clones.jobs.get(taskId.value)
    if (previous) {
      const session = sessions.sessions.get(previous.targetConnectionId)
      connectionKey.value = session?.connectionKey ?? ''
      db.value = session?.db ?? 0
      name.value = previous.targetKey
      submitted.value = { sessionId: previous.targetConnectionId, key: previous.targetKey }
      applyJob()
    }
  }
  await nextTick()
  restoringForm = false
  nameInput.value?.focus()
  nameInput.value?.setSelectionRange(name.value.length, name.value.length)
  if (next.mode === 'clone') {
    try {
      const meta = await getKeyMeta(next.sessionId, next.key)
      if (target.value === next) sourceType.value = meta.keyType
    } catch { /* 源的最终存在性由执行时的原子快照判定。 */ }
  }
})

watch(connectionKey, (value, previous) => {
  if (value === previous || restoringForm || running.value) return
  db.value = value === source.value?.connectionKey ? source.value.db : 0
})
watch([connectionKey, db], ([key, value], [oldKey, previous]) => {
  const origin = target.value
  if (origin?.mode !== 'clone' || restoringForm || running.value) return
  const oldSuggestion = oldKey === source.value?.connectionKey && previous === source.value?.db ? `${origin.key}_copy` : origin.key
  if (name.value === oldSuggestion) name.value = key === source.value?.connectionKey && value === source.value?.db ? `${origin.key}_copy` : origin.key
})
watch([name, db, connectionKey], () => { if (!restoringForm && !uncertain.value && !written.value && !running.value) { error.value = ''; conflict.value = false; taskId.value = null } })

function applyJob() {
  const current = job.value
  if (!current) return
  if (current.phase === 'completed') {
    written.value = true
    if (current.taskId === closeOnSuccess.value) ui.keyWriteDialog = null
  } else if (current.phase === 'failed' || current.phase === 'uncertain') {
    error.value = current.message
    conflict.value = current.errorCode === 'redis.key_exists'
    uncertain.value = current.phase === 'uncertain'
  }
}
watch(job, applyJob)

async function cancelClone() {
  if (!taskId.value) return
  try { await tasks.cancel(taskId.value) } catch (cause) { ui.notifyError(cause, '停止克隆失败') }
}

async function targetSession(): Promise<string> {
  const origin = source.value
  if (!origin) throw new Error('源会话已关闭，请重新连接')
  if (!Number.isInteger(db.value) || db.value < 0 || (targetCluster.value && db.value !== 0)) throw new Error('目标数据库无效')
  if (sameConnection.value && db.value === origin.db) return origin.id
  if (!connectionKey.value) throw new Error('请选择目标连接')
  return sessions.openDb(connectionKey.value, db.value)
}

async function showTarget() {
  const saved = submitted.value
  if (!saved) return
  if (!sessions.sessions.has(saved.sessionId)) { error.value = '目标会话已关闭，请重新连接后查看 Key'; return }
  tabs.open({ kind: 'key', sessionId: saved.sessionId, key: saved.key, preview: false })
  open.value = false
}

async function checkTarget() {
  const saved = submitted.value
  if (!saved) return
  busy.value = true
  try {
    const exists = await keyExists(saved.sessionId, saved.key)
    if (!exists) {
      uncertain.value = false
      conflict.value = false
      error.value = '目标 Key 当前不存在。可重新提交；若先前请求稍后完成，同名保护会阻止覆盖。'
    } else {
      conflict.value = true
      error.value = '目标 Key 已存在，请打开检查内容；无法仅凭存在判断是否由本次操作创建。'
    }
  } catch (cause) { error.value = toFault(cause).text }
  finally { busy.value = false }
}

async function submit() {
  if (!canSubmit.value || !target.value) return
  busy.value = true
  error.value = ''
  conflict.value = false
  const origin = target.value
  const key = name.value // 不 trim：空格与空字符串都是合法 Key 名。
  let writing = false
  try {
    const expiry = keyExpiry(draft.value, origin.mode === 'clone' && preserve.value)
    const value = origin.mode === 'create' ? initialValue(type.value, string.value, rows.value) : null
    const sessionId = await targetSession()
    submitted.value = { sessionId, key }
    if (sessions.sessions.get(sessionId)?.readonly) throw new Error('目标数据库会话为只读，无法写入')
    writing = true
    if (origin.mode === 'clone') {
      await tasks.start()
      const started = await startCloneKey(origin.sessionId, sessionId, origin.key, key, expiry)
      taskId.value = started.taskId
      closeOnSuccess.value = started.taskId
      clones.track(started.taskId, openAfter.value)
      return
    }
    const result = await createKey(sessionId, key, value!, expiry)
    writing = false
    written.value = true
    sessions.invalidateKeyspace(sessionId)
    const destinationSession = sessions.sessions.get(sessionId)
    if (destinationSession && (destinationSession.searchQuery || destinationSession.searchScope !== null || destinationSession.pattern)) {
      void sessions.rescan(sessionId)
      ui.notify('info', '列表保留当前筛选条件；如未显示新 Key，请按完整 Key 名搜索')
    } else {
      sessions.rememberKey(sessionId, key, result.keyType)
    }
    if (destinationSession?.connectionKey) void sessions.loadDbList(destinationSession.connectionKey, sessionId)
    tabs.open({ kind: 'key', sessionId, key, keyType: result.keyType, preview: false })
    ui.notify('ok', '已创建 Key')
    ui.keyWriteDialog = null
  } catch (cause) {
    const fault = toFault(cause)
    if (written.value) {
      conflict.value = true
      error.value = `Key 已写入，但界面刷新失败，请打开目标检查。${fault.text}`
      return
    }
    conflict.value = fault.code === 'redis.key_exists'
    // 服务端错误以外的通信异常可能发生在写入已完成之后；保留目标供核对。
    uncertain.value = writing && !conflict.value && (
      fault.code === 'frontend.unexpected' || /timeout|timed out|connection|broken pipe|I\/O|network|closed|reset/i.test(fault.text)
    )
    error.value = uncertain.value ? `结果待确认，请先检查目标 Key。${fault.text}` : fault.text
  } finally { busy.value = false }
}
</script>

<template>
  <Dlg v-model:open="open" :title="cloning ? '克隆 Key' : '新建 Key'" :description="scopeLabel" :busy="busy">
    <div class="form">
      <!-- 写入中、结果待确认、已写入：三种状态都不允许再改表单，fieldset 一次禁掉 -->
      <fieldset class="keywrite-fieldset" :disabled="busy || running || uncertain || written">
        <div v-if="target?.mode === 'clone'" class="form-sec">
          <div class="cap">来源</div>
          <div class="field">
            <label>Key</label>
            <div class="field-ctl">
              <span class="keywrite-key mono selectable">{{ target.key }}</span>
              <TypeChip v-if="sourceType" :type="sourceType" />
            </div>
            <p class="field-hint">复制执行时服务端的完整数据，未保存的编辑内容不会包含在副本中。</p>
          </div>
        </div>

        <div class="form-sec">
          <div class="cap">目标</div>
          <div v-if="cloning" class="field">
            <label for="keywrite-connection">连接</label>
            <div class="field-ctl">
              <div class="select grow">
                <select id="keywrite-connection" v-model="connectionKey" class="input">
                  <option v-if="!source?.connectionKey" value="">{{ source?.name ?? '当前连接' }}</option>
                  <option v-for="item in connections.items" :key="item.key" :value="item.key">{{ item.name }} · {{ item.host }}:{{ item.port }}</option>
                </select>
                <span class="caret"><Icon name="chevron-down" :size="12" /></span>
              </div>
            </div>
            <p class="field-hint">目标收到完整数据后校验序列化格式。同名或不兼容时停止。</p>
          </div>
          <div class="field">
            <label>数据库</label>
            <div class="field-ctl">
              <!-- 新建时 DB 由入口决定（doc/frontend/key-write-dialog.md「入口」），只有克隆可以改 -->
              <template v-if="cloning">
                <input
                  v-model.number="db"
                  class="input w-88 num"
                  type="number"
                  min="0"
                  :max="targetCluster ? 0 : undefined"
                  :disabled="targetCluster"
                  aria-label="目标数据库"
                />
                <span v-if="targetCluster" class="muted" style="font-size: var(--fs-11)">集群模式只有 db0</span>
              </template>
              <span v-else class="mono">db{{ db }}</span>
              <span v-if="destination?.readonly" class="tag tag-danger">只读</span>
            </div>
          </div>
          <div class="field">
            <label for="keywrite-name">Key 名</label>
            <div class="field-ctl">
              <input id="keywrite-name" ref="nameInput" v-model="name" class="input mono" spellcheck="false" />
            </div>
            <p v-if="name === ''" class="field-hint">当前名称为空字符串，将创建空名称 Key。</p>
          </div>
        </div>

        <div v-if="!cloning" class="form-sec">
          <div class="cap">初始内容</div>
          <div class="field">
            <label for="keywrite-type">类型</label>
            <div class="field-ctl">
              <div class="select">
                <select id="keywrite-type" v-model="type" class="input w-120">
                  <option v-for="kind in CREATE_TYPES" :key="kind" :value="kind">{{ kind.toUpperCase() }}</option>
                </select>
                <span class="caret"><Icon name="chevron-down" :size="12" /></span>
              </div>
            </div>
          </div>

          <div v-if="type === 'string'" class="field" style="align-items: start">
            <label>初始值</label>
            <div class="field-ctl">
              <KeyBytesInput v-model="string" label="初始值" :rows="6" />
            </div>
          </div>

          <template v-else>
            <div class="keywrite-rows">
              <div v-for="(row, index) in rows" :key="row.id" class="keywrite-row">
                <span class="keywrite-idx">{{ index + 1 }}</span>
                <KeyBytesInput v-if="hasField" v-model="row.field" :label="`字段 ${index + 1}`" placeholder="字段" />
                <KeyBytesInput
                  v-model="row.value"
                  :label="`${hasField ? '值' : '成员'} ${index + 1}`"
                  :placeholder="hasField ? '值' : '成员'"
                />
                <input
                  v-if="type === 'zset'"
                  v-model="row.score"
                  class="input mono num w-88"
                  placeholder="score"
                  :aria-label="`score ${index + 1}`"
                />
                <Btn
                  variant="ghost"
                  icon="x"
                  icon-only
                  size="sm"
                  :title="`移除第 ${index + 1} 行`"
                  :disabled="rows.length === 1"
                  @click="rows.splice(index, 1)"
                />
              </div>
            </div>
            <div class="keywrite-rows-ft">
              <Btn size="sm" icon="plus" :disabled="rows.length >= 256" @click="rows.push(newRow())">添加一行</Btn>
              <span class="field-hint">
                {{ type === 'stream' ? '作为第一条消息写入，ID 由服务端生成' : '至少一行，最多 256 行' }}
              </span>
            </div>
          </template>
        </div>

        <div class="form-sec">
          <div class="cap">过期时间</div>
          <div class="pop-body" style="padding: 0">
            <label v-if="cloning" class="pop-opt">
              <Cbx v-model="preserve" />
              <span>{{ sameConnection ? '保留源 Key 的到期时刻（永久 Key 仍为永久）' : '保留剩余有效期，扣除传输耗时（永久 Key 仍为永久）' }}</span>
            </label>
            <!-- 不显示 PERSIST / EXPIRE 提示：TTL 和内容在同一个脚本里落地，
                 单独列一条命令名会让人以为会多发一条 -->
            <TtlModePicker v-if="customTtl" v-model="draft" hide-command-hints />
          </div>
          <p v-if="customTtl && resultAt" class="field-hint" style="grid-column: auto; margin-top: 6px">
            到期于 <b class="num">{{ formatDateTime(resultAt) }}</b>
          </p>
        </div>

        <div v-if="cloning" class="form-sec">
          <div class="cap">完成后</div>
          <label class="pop-opt">
            <Cbx v-model="openAfter" />
            <span>打开副本</span>
          </label>
        </div>
      <div v-if="job" class="form-sec">
        <div class="cap">克隆任务</div>
        <div class="field">
          <label>状态</label>
          <div class="field-ctl">
            <Icon v-if="running" name="loader" :size="12" />
            <span>{{ job.message }}</span>
            <span v-if="job.bytes !== null" class="muted num">{{ formatBytes(job.bytes) }}</span>
          </div>
          <p class="field-hint">{{ job.targetName }} · <span class="mono">{{ job.targetKey }}</span></p>
          <p v-if="running" class="field-hint">可关闭此窗口，稍后从状态栏「克隆任务」查看结果。</p>
        </div>
      </div>
      </fieldset>

      <!-- 能力不足与失败都用横幅说，不要等按下去才知道（连接对话框同一做法） -->
      <div v-if="!source" class="banner banner-danger" style="margin-top: 14px">
        <Icon name="alert-triangle" :size="16" />
        <span>源会话已关闭，请重新连接。</span>
      </div>
      <div v-else-if="destination?.readonly" class="banner banner-danger" style="margin-top: 14px">
        <Icon name="lock" :size="16" />
        <span>目标会话为只读，无法写入。</span>
      </div>
      <div v-else-if="scriptingBlocked" class="banner banner-danger" style="margin-top: 14px">
        <Icon name="alert-triangle" :size="16" />
        <span>{{ scriptingBlocked }}</span>
      </div>
      <div v-if="error" class="banner banner-danger selectable" style="margin-top: 14px" role="alert">
        <Icon name="alert-triangle" :size="16" />
        <span>{{ error }}</span>
      </div>
    </div>

    <template #footer>
      <!-- 页脚左侧：平时说后果；出了冲突或结果待确认，就换成对应的动作 -->
      <Btn v-if="uncertain" :disabled="busy" @click="checkTarget">检查目标 Key</Btn>
      <Btn v-if="conflict" :disabled="busy" @click="showTarget">打开已有 Key</Btn>
      <Btn v-if="written && cloning" :disabled="busy" @click="showTarget">打开副本</Btn>
      <span v-if="!uncertain && !conflict" class="bulk-note">目标已有同名 Key 时停止，不覆盖已有数据</span>
      <span class="spacer" />
      <Btn v-if="running" :disabled="job?.phase === 'restoring'" @click="cancelClone">停止克隆</Btn>
      <Btn :disabled="busy" @click="open = false">{{ running ? '后台运行' : taskId ? '关闭' : '取消' }}</Btn>
      <Btn v-if="!running && !written" variant="primary" :disabled="!canSubmit" :icon="busy ? 'loader' : undefined" @click="submit">
        {{ busy ? '处理中…' : cloning ? '克隆' : '创建并打开' }}
      </Btn>
    </template>
  </Dlg>
</template>
