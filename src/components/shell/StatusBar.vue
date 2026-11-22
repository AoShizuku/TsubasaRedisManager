<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import Menu from '@/components/ui/Menu.vue'
import MenuItem from '@/components/ui/MenuItem.vue'
import { useClonesStore } from '@/stores/clones'
import Icon from '@/components/ui/Icon.vue'
import { getServerMemory } from '@/api/redis'
import { formatApproxPercent, formatCount, formatPartialCount } from '@/lib/format'
import { useSessionsStore } from '@/stores/sessions'
import { useSettingsStore } from '@/stores/settings'
import { useTabsStore } from '@/stores/tabs'
import { useTasksStore } from '@/stores/tasks'
import { useUiStore } from '@/stores/ui'

const sessions = useSessionsStore()
const tasks = useTasksStore()
const tabs = useTabsStore()
const settings = useSettingsStore()
const ui = useUiStore()
const clones = useClonesStore()

const session = computed(() => sessions.activeSession)

/** 任务类型是后端的标识符，直接显示会让中文界面上蹦出 `batch_ttl`。 */
const TASK_TYPE_LABEL: Record<string, string> = {
  load_all_keys: '加载全部 Key',
  search_keys: '搜索 Key',
  batch_delete: '批量删除',
  batch_ttl: '批量设置 TTL',
  // 试算是独立的任务类型。共用「批量删除」那一行的话，点一下试算就会看到
  // 「批量删除 正在运行」—— 那一秒用户会以为自己点错按钮把库删了
  batch_delete_dry_run: '批量删除（试算）',
  batch_ttl_dry_run: '批量设置 TTL（试算）',
  memory_analysis: '内存分析',
  monitor: 'Monitor',
  subscribe: '订阅',
  psubscribe: '订阅',
  import_keys: '导入 Key',
  import_commands: '导入命令',
  export_keys: '导出 Key',
  clone_key: '克隆 Key',
}

function taskLabel(taskType: string): string {
  return TASK_TYPE_LABEL[taskType] ?? taskType
}

const memory = ref<{ used: string; max: string | null } | null>(null)

/** INFO memory 是廉价的单条命令，但也没必要轮询：切会话时取一次，用户可点刷新。 */
async function loadMemory() {
  const current = session.value
  if (!current) {
    memory.value = null
    return
  }
  try {
    const info = await getServerMemory(current.id)
    memory.value = info.used ? { used: info.used, max: info.max } : null
  } catch {
    memory.value = null
  }
}

watch(() => session.value?.id, loadMemory, { immediate: true })

const scanText = computed(() => {
  const current = session.value
  if (!current) return null
  if (current.scanning) return `扫描中 ${formatCount(current.keys.length)}`
  return `${formatPartialCount(current.keys.length, current.scanComplete)} 个 Key`
})

/** 内存读数目前是唯一能一眼看到"库有多大"的地方，让它可点就是排查大 key 的最短路径。 */
function openMemoryBrowser() {
  const current = session.value
  if (!current) return
  settings.state.analysisMode = true
  tabs.open({ kind: 'browser', sessionId: current.id, pattern: '*', sortByMemory: true })
}
</script>

<template>
  <div class="statusbar">
    <template v-if="session">
      <span class="sb">
        <span class="dot" :class="session.error ? 'dot-err' : 'dot-ok'" />
        <span class="clip">{{ session.name }}</span>
      </span>
      <span class="sb">db{{ session.db }}</span>
      <span v-if="session.readonly" class="sb">
        <Icon name="lock" :size="12" />
        只读
      </span>
      <span v-if="session.cluster" class="sb">集群</span>

      <span class="sb-sep" />
      <span class="sb num">{{ scanText }}</span>
      <!-- 「继续」属于浏览态的游标扫描；搜索态任务自己在跑，scanMore 也会
           被 store 的搜索守卫弹回 —— 一个点了没反应的按钮比没有更糟 -->
      <button
        v-if="!session.scanComplete && !session.scanning && session.searchStatus === 'idle'"
        class="sb"
        title="继续扫描下一批"
        @click="sessions.scanMore(session.id)"
      >
        <Icon name="chevron-right" :size="12" />
        继续
      </button>
    </template>
    <span v-else class="sb muted">未连接</span>

    <template v-if="tasks.runningCount > 0">
      <span class="sb-sep" />
      <span class="sb">
        <Icon name="loader" :size="12" />
        <span class="num">{{ tasks.runningCount }} 个后台任务</span>
      </span>
      <button
        v-for="task in tasks.running"
        :key="task.taskId"
        class="sb"
        :title="`停止${taskLabel(task.taskType)}`"
        :disabled="task.taskType === 'clone_key' && clones.jobs.get(task.taskId)?.phase === 'restoring'"
        @click="tasks.cancel(task.taskId).catch((e) => ui.notifyError(e, '停止任务失败'))"
      >
        <Icon name="ban" :size="12" />
        {{ taskLabel(task.taskType) }}
        <span v-if="task.taskType === 'clone_key'">{{ task.message }}</span>
        <span v-else class="num">{{ formatCount(task.processed) }}</span>
        <!-- 扫描类任务的近似进度（doc/backend/tasks.md「扫描任务：只报命中数」）：只有它们带这个字段，
             批量删除等有精确 total 的任务恒为 null，不进这里 -->
        <span v-if="task.estimatedProgress !== null" class="num">
          {{ formatApproxPercent(task.estimatedProgress) }}
        </span>
      </button>
    </template>

    <span class="spacer" />
    <Menu v-if="clones.jobs.size" side="top" align="end">
      <template #trigger><button class="sb">克隆任务</button></template>
      <MenuItem v-for="job in [...clones.jobs.values()].reverse()" :key="job.taskId" icon="copy" @select="ui.openCloneKey(job.sourceConnectionId, job.sourceKey, job.taskId)">
        {{ job.targetName }} · {{ job.targetKey }} · {{ job.message }}
      </MenuItem>
    </Menu>

    <button
      v-if="memory"
      class="sb"
      title="按内存占用打开 Key 浏览器"
      @click="openMemoryBrowser"
    >
      <Icon name="chart" :size="12" />
      <span class="num">{{ memory.used }}<template v-if="memory.max"> / {{ memory.max }}</template></span>
    </button>
    <button v-if="session" class="sb" title="刷新内存读数" @click="loadMemory">
      <Icon name="refresh" :size="12" />
    </button>
  </div>
</template>
