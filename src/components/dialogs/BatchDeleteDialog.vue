<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Dlg from '@/components/ui/Dlg.vue'
import Icon from '@/components/ui/Icon.vue'
import { startBatchDelete } from '@/api/redis'
import { formatCount } from '@/lib/format'
import { useSessionsStore } from '@/stores/sessions'
import { useSettingsStore } from '@/stores/settings'
import { useTasksStore } from '@/stores/tasks'
import { useUiStore } from '@/stores/ui'
import type { BatchDeleteTaskOptions } from '@/types/ipc'

/**
 * 批量删除 Key。
 *
 * 和批量 TTL 同一个骨架，因为它们是同一类操作：低频、高风险、没有撤销。
 * 删除没有参数可调，所以只剩「作用范围」一块 —— 剩下的全部重量都压在
 * 「到底会删掉哪些、多少个」上。规则见 doc/frontend/batch.md。
 *
 * **不强制先试算。** 试算是完整跑一遍 SCAN，对一个百万级前缀等于把工作量翻倍；
 * 而 doc/frontend/batch.md「批量不是 for 循环」认定批量删除的破坏「至少是当场可见的」（删完树里就少了），
 * 比批量 TTL 那种延迟且不可见的破坏轻 —— 批量 TTL 都没强制，这里更没理由。
 * 危险靠按钮文案说清楚，不靠多加一道门。
 *
 * **也不要再套一层 `ui.confirm`。** 两层模态是噪声，第二层几乎一定被无脑点掉，
 * 反而稀释了第一层的注意力。这个对话框本身就是那次确认。
 */
const sessions = useSessionsStore()
const settings = useSettingsStore()
const tasks = useTasksStore()
const ui = useUiStore()

const target = computed(() => ui.batchDeleteDialog)
const open = computed({
  get: () => !!target.value,
  set: (value: boolean) => {
    if (!value) ui.closeBatchDelete()
  },
})

const scope = ref<'keys' | 'pattern'>('pattern')
const pattern = ref('')
const busy = ref(false)
const estimating = ref(false)
/** 试算出的命中数。null = 还没算过，模式模式下数量因此是未知的。 */
const estimated = ref<number | null>(null)

/** 每次打开都从目标重置，避免残留上一次的输入。 */
watch(target, (next) => {
  if (!next) return
  scope.value = next.scope
  pattern.value = next.pattern
  estimated.value = null
  busy.value = false
  estimating.value = false
})

/** 改了模式，上一次的试算数字就不再对应任何东西。 */
watch(pattern, () => {
  estimated.value = null
})

const session = computed(() =>
  target.value ? sessions.sessions.get(target.value.sessionId) ?? null : null,
)
const scopeLabel = computed(() =>
  session.value ? `${session.value.name} / db${session.value.db}` : '',
)

const selectedCount = computed(() => target.value?.keys.length ?? 0)

/** 显式列表的数量是确定的；模式的数量只有试算过才知道。不要编。 */
const affected = computed<number | null>(() =>
  scope.value === 'keys' ? selectedCount.value : estimated.value,
)

/**
 * `*` 匹配这个 DB 里的每一个 key —— 这不是「删一批」，是清空整个 DB。
 * 数字说不出这件事（试算出来的一千万只是个大数字），DB 号说得出来。
 */
const wipesDb = computed(() => scope.value === 'pattern' && pattern.value.trim() === '*')
const dbLabel = computed(() => (session.value ? `db${session.value.db}` : '当前 DB'))

/**
 * 按钮上要写清楚这一下会删掉什么。删除永远危险，所以颜色不变（永远是红的），
 * 变的是它说了多少 —— 数量确定就写数字，不确定就承认不确定。见 doc/frontend/batch.md「危险写在按钮上」。
 */
const submitLabel = computed(() => {
  if (wipesDb.value) return `删除 ${dbLabel.value} 的全部 Key`
  if (affected.value === null) return '删除匹配的全部 Key（数量未知）'
  return `删除 ${formatCount(affected.value)} 个 Key`
})

const preview = computed(() =>
  scope.value === 'keys'
    ? 'DEL <key>'
    : `SCAN MATCH ${pattern.value.trim() || '<模式>'} → DEL`,
)

/**
 * 删完要从树里裁掉的字面前缀，没有就是 null。
 *
 * 三个条件缺一不可，而且**模式没被编辑过**是最容易漏掉的那个：`prefix` 是
 * 打开时那个模式的字面版本，用户把 `cache:page:*` 改成 `cache:*` 再删，
 * 它就不再对应实际删掉的范围了 —— 照旧裁 `cache:page:` 那一片，
 * 树上会留下一堆已经不存在的 key，比不裁更糟。
 */
const prunePrefix = computed<string | null>(() => {
  const current = target.value
  if (!current || current.prefix === null) return null
  if (scope.value !== 'pattern' || pattern.value !== current.pattern) return null
  return current.prefix
})

const scopeReady = computed(() =>
  scope.value === 'keys' ? selectedCount.value > 0 : pattern.value.trim().length > 0,
)
const canSubmit = computed(() => !busy.value && scopeReady.value && !session.value?.readonly)

/** 只对模式开放 —— 显式列表的数量本来就是确定的，没什么可试算。 */
const canEstimate = computed(() => !estimating.value && pattern.value.trim().length > 0)

/* ── 执行 ─────────────────────────────────────────────── */

function optionsFor(dryRun: boolean): BatchDeleteTaskOptions {
  const keys = target.value?.keys ?? []
  return {
    keys:
      scope.value === 'keys' ? keys.map((key) => ({ type: 'string' as const, value: key })) : [],
    patterns: scope.value === 'pattern' ? [pattern.value.trim()] : [],
    dryRun,
    count: settings.state.scanCount,
  }
}

/**
 * 试算走的是同一个任务、同一份参数，只是 dryRun —— 扫描路径完全相同。
 * 换一条路去数一遍就没意义了：试算的全部价值在于
 * 「试算出来的数字就是真会删的数字」。
 */
async function estimate() {
  const current = target.value
  if (!current || estimating.value || scope.value !== 'pattern' || !canEstimate.value) return
  estimating.value = true
  estimated.value = null
  try {
    const started = await startBatchDelete(current.sessionId, optionsFor(true))
    const summary = await tasks.watchSettled(started.taskId)
    // 试算不删东西，命中数走 processed
    estimated.value = summary.status === 'completed' ? summary.processed : null
    if (summary.status !== 'completed') ui.notify('warn', '试算未完成')
  } catch (error) {
    ui.notifyError(error, '试算失败')
  } finally {
    estimating.value = false
  }
}

async function submit() {
  const current = target.value
  if (!current || !canSubmit.value) return
  busy.value = true
  // 关了对话框 target 就是 null，computed 跟着塌掉 —— 先把它取出来
  const prefix = prunePrefix.value
  try {
    const started = await startBatchDelete(current.sessionId, optionsFor(false))
    // 任务一启动就关对话框 —— 不要让一个 modal 在那里挂几分钟。
    // 进度归状态栏的任务条，结果归 toast。
    ui.closeBatchDelete()
    void report(started.taskId, current.sessionId, prefix)
  } catch (error) {
    ui.notifyError(error, '批量删除失败')
  } finally {
    busy.value = false
  }
}

/**
 * 报结果。
 *
 * **不报 `processed - succeeded` 这个差值。** 它的含义是「扫到的时候还在，
 * 发 DEL 的时候已经不在了」—— 对用户想做的事没有任何影响，那个 key 现在
 * 确实不在库里。批量 TTL 得报「未变更」是因为它的差值代表条件没命中（doc/backend/tasks.md「进度口径」），
 * 而这里界面上根本不显示 processed，说了反而凭空造出一个要去查的疑问。
 */
async function report(taskId: string, sessionId: string, prefix: string | null) {
  const summary = await tasks.watchSettled(taskId)
  if (summary.status === 'failed') {
    ui.notify('danger', '批量删除失败', summary.message)
  } else if (summary.status === 'cancelled') {
    ui.notify('warn', `已停止，已删除 ${formatCount(summary.succeeded)} 个 Key`)
  } else {
    ui.notify('ok', `已删除 ${formatCount(summary.succeeded)} 个 Key`)
  }

  // 三种结局都要刷：停下之前删掉的是真删了，报错之前删掉的也是。
  // 打开着的标签页读数会陈旧，表格里也还留着已经不存在的行。
  // 只刷读数，不动标签状态 —— 「写入即转正」的判据是"你对**它**动过手"，
  // 批量是对一个范围动手，把碰巧开着的预览页转正会莫名其妙（doc/frontend/tabs.md「写入即转正」）。
  sessions.invalidateKeyspace(sessionId)

  // 树不跟着版本号刷新（它是显式加载的快照），所以从树进来的删除要自己把
  // 那一片摘掉 —— doc/frontend/batch.md「批量不是 for 循环」认定批量删除的破坏「至少是当场可见的」，
  // 树上原封不动就把这句话变成了假话。
  //
  // **只有跑完了才裁。** cancelled / failed 意味着只删了一半，把整片抹掉
  // 就是撒谎；那两种情况维持现状，用户可以用树上的「重新扫描」对齐。
  if (prefix !== null && summary.status === 'completed') {
    sessions.forgetPrefix(sessionId, prefix)
  }
}
</script>

<template>
  <Dlg
    v-if="target"
    v-model:open="open"
    title="批量删除 Key"
    :description="scopeLabel"
    :width="520"
    :busy="busy"
  >
    <div class="form">
      <div class="form-sec">
        <div class="cap">作用范围</div>

        <label class="pop-opt">
          <span
            class="radio"
            :class="{ on: scope === 'keys' }"
            @click="selectedCount > 0 && (scope = 'keys')"
          />
          <span :class="{ muted: selectedCount === 0 }">
            已选中的 <b class="num">{{ formatCount(selectedCount) }}</b> 个 Key
          </span>
          <span v-if="selectedCount === 0" class="muted" style="font-size: var(--fs-11)">
            （没有勾选任何 Key）
          </span>
        </label>

        <label class="pop-opt">
          <span class="radio" :class="{ on: scope === 'pattern' }" @click="scope = 'pattern'" />
          <span>匹配模式</span>
          <input
            v-model="pattern"
            class="input mono grow"
            placeholder="例如 cache:page:*"
            @focus="scope = 'pattern'"
          />
          <Btn
            size="sm"
            :icon="estimating ? 'loader' : 'search'"
            :disabled="scope !== 'pattern' || !canEstimate"
            title="扫一遍但不删任何东西，只数出会删掉多少个 Key"
            @click="estimate"
          >
            试算
          </Btn>
        </label>

        <!-- 模式的命中数在 SCAN 之前是未知的。不要编一个数字出来。 -->
        <p v-if="scope === 'pattern'" class="field-hint" style="grid-column: auto; margin-top: 4px">
          <template v-if="estimated !== null">
            试算命中 <b class="num">{{ formatCount(estimated) }}</b> 个 Key
          </template>
          <template v-else>
            数量未知，执行中可随时在状态栏停止
          </template>
        </p>
      </div>
    </div>

    <template #footer>
      <div class="bulk-note">
        <!-- 危险要在按下之前就说出来，而且要说实话：删除没有撤销 -->
        <span class="bulk-danger">
          <Icon name="alert-triangle" :size="12" />
          这些 Key 会被立即删除，不可撤销
        </span>
        <span class="bulk-cmd mono">{{ preview }}</span>
      </div>

      <span class="spacer" />
      <Btn :disabled="busy" @click="ui.closeBatchDelete()">取消</Btn>
      <!-- 整句包一个 .num：.btn 是 flex，把数字单独拆成一个子元素会被 gap 撑开 -->
      <Btn variant="dangerSolid" :disabled="!canSubmit" @click="submit">
        <span class="num">{{ submitLabel }}</span>
      </Btn>
    </template>
  </Dlg>
</template>
