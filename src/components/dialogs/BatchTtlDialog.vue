<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Dlg from '@/components/ui/Dlg.vue'
import Icon from '@/components/ui/Icon.vue'
import TtlModePicker from '@/components/keys/TtlModePicker.vue'
import { startBatchTtl } from '@/api/redis'
import { formatCount, formatDateTime, formatSpan } from '@/lib/format'
import {
  newTtlDraft,
  ttlCommandFor,
  useTtlDraft,
  TTL_CONDITIONS,
  type TtlCondition,
} from '@/lib/ttl'
import { useSessionsStore } from '@/stores/sessions'
import { useSettingsStore } from '@/stores/settings'
import { useTasksStore } from '@/stores/tasks'
import { useUiStore } from '@/stores/ui'
import type { BatchTtlTaskOptions } from '@/types/ipc'

/**
 * 批量设置过期时间。
 *
 * 单 key 的 TTL 编辑是 popover（高频、低风险）；批量把这两个判据都反了过来，
 * 而且它的破坏是**延迟发生、不可见、不可撤销**的 —— 点下去界面什么都不变，
 * 一分钟后库空了。所以它是 modal，而且危险要在按钮上写出来。
 * 判据与全部规则见 doc/frontend/batch.md。
 */
const sessions = useSessionsStore()
const settings = useSettingsStore()
const tasks = useTasksStore()
const ui = useUiStore()

const target = computed(() => ui.batchTtlDialog)
const open = computed({
  get: () => !!target.value,
  set: (value: boolean) => {
    if (!value) ui.closeBatchTtl()
  },
})

const scope = ref<'keys' | 'pattern'>('pattern')
const pattern = ref('')
const draft = ref(newTtlDraft(null))
const condition = ref<TtlCondition | null>(null)
const busy = ref(false)
const estimating = ref(false)
/** 试算出的命中数。null = 还没算过，模式模式下数量因此是未知的。 */
const estimated = ref<number | null>(null)

const { seconds, absoluteMs, resultAt, invalid, value } = useTtlDraft(draft)

/** 每次打开都从目标重置，避免残留上一次的输入。 */
watch(target, (next) => {
  if (!next) return
  scope.value = next.scope
  pattern.value = next.pattern
  draft.value = newTtlDraft(null)
  condition.value = null
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

/* ── 条件 flag ────────────────────────────────────────── */

/**
 * 能力 expireConditions。unknown 算不算能用由后端注册表决定（这一项是「未知时禁用」），
 * 前端只认 usable；提示原样用后端的 reason —— 判断依据只有后端知道，自己拼「该服务端是
 * Redis x」在 Valkey 上就是错的。见 doc/backend/capabilities.md。
 */
const conditionsCapability = computed(() => session.value?.capabilities.expireConditions ?? null)
const conditionsSupported = computed(() => conditionsCapability.value?.usable ?? false)
/** PERSIST 没有条件 flag，服务端不支持时也没有 —— 两种情况都整块禁用。 */
const conditionsUsable = computed(() => conditionsSupported.value && draft.value.mode !== 'persist')
const effectiveCondition = computed(() => (conditionsUsable.value ? condition.value : null))
const conditionHint = computed(() =>
  conditionsSupported.value ? null : (conditionsCapability.value?.reason ?? null),
)

/* ── 危险 ─────────────────────────────────────────────── */

/**
 * 到期时刻落在「即将过期」阈值内时，这一批 key 马上就会消失。
 * 返回还剩多少秒，用来把话说全。
 */
const imminentIn = computed<number | null>(() => {
  if (draft.value.mode === 'persist' || invalid.value) return null
  const at = resultAt.value
  if (!at) return null
  const span = (at.getTime() - Date.now()) / 1000
  return span <= settings.state.ttlSoonThreshold ? span : null
})

const affectedLabel = computed(() =>
  affected.value === null ? '匹配的 Key' : `${formatCount(affected.value)} 个 Key`,
)

const submitLabel = computed(() => {
  if (draft.value.mode === 'persist') return '取消过期时间'
  const span = imminentIn.value
  if (span === null) return '设置 TTL'
  return `设置 TTL（${affectedLabel.value}将在 ${formatSpan(span)}内消失）`
})

const preview = computed(
  () =>
    ttlCommandFor(
      draft.value.mode,
      '<key>',
      seconds.value,
      absoluteMs.value,
      effectiveCondition.value,
    ).preview,
)

const scopeReady = computed(() =>
  scope.value === 'keys' ? selectedCount.value > 0 : pattern.value.trim().length > 0,
)
const canSubmit = computed(
  () => !busy.value && !invalid.value && scopeReady.value && !session.value?.readonly,
)

/**
 * 试算也要过后端那道校验（过去的绝对时刻会被拒），所以参数无效时它同样点不动。
 * 这也符合直觉：参数还不对，什么都不该发出去。
 */
const canEstimate = computed(
  () => !estimating.value && !invalid.value && pattern.value.trim().length > 0,
)

/* ── 执行 ─────────────────────────────────────────────── */

function optionsFor(dryRun: boolean): BatchTtlTaskOptions {
  const keys = target.value?.keys ?? []
  return {
    keys:
      scope.value === 'keys' ? keys.map((key) => ({ type: 'string' as const, value: key })) : [],
    patterns: scope.value === 'pattern' ? [pattern.value.trim()] : [],
    mode: draft.value.mode,
    value: value.value,
    condition: effectiveCondition.value,
    dryRun,
    count: settings.state.scanCount,
  }
}

/**
 * 试算走的是同一个任务、同一份参数，只是 dryRun —— 扫描路径完全相同。
 * 换一条路去数一遍就没意义了：试算的全部价值在于
 * 「试算出来的数字就是真会改的数字」。
 */
async function estimate() {
  const current = target.value
  // 只对模式试算 —— 显式列表的数量本来就是确定的
  if (!current || estimating.value || scope.value !== 'pattern' || !canEstimate.value) return
  estimating.value = true
  estimated.value = null
  try {
    const started = await startBatchTtl(current.sessionId, optionsFor(true))
    const summary = await tasks.watchSettled(started.taskId)
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
  try {
    const started = await startBatchTtl(current.sessionId, optionsFor(false))
    // 任务一启动就关对话框 —— 不要让一个 modal 在那里挂几分钟。
    // 进度归状态栏的任务条，结果归 toast。
    ui.closeBatchTtl()
    void report(started.taskId, current.sessionId)
  } catch (error) {
    ui.notifyError(error, '批量设置过期时间失败')
  } finally {
    busy.value = false
  }
}

/**
 * 报结果。**「未变更」必须说出来** —— 勾了 100 个看到「成功 87」会让人
 * 以为漏了 13 个，然后去翻日志找一个不存在的 bug。见 doc/backend/tasks.md「进度口径」。
 */
async function report(taskId: string, sessionId: string) {
  const summary = await tasks.watchSettled(taskId)
  if (summary.status === 'failed') {
    ui.notify('danger', '批量设置过期时间失败', summary.message)
    return
  }
  if (summary.status === 'cancelled') {
    // 停下之前已经改掉的那些是真的改了，读数照样要刷
    ui.notify('warn', `已停止，已设置 ${formatCount(summary.succeeded)} 个 Key`)
    sessions.invalidateKeyspace(sessionId)
    return
  }

  const unchanged = Math.max(0, summary.processed - summary.succeeded - summary.failed)
  const parts = [`已设置 ${formatCount(summary.succeeded)} 个`]
  if (unchanged > 0) parts.push(`未变更 ${formatCount(unchanged)} 个`)
  if (summary.failed > 0) parts.push(`失败 ${formatCount(summary.failed)} 个`)
  ui.notify(summary.failed > 0 ? 'danger' : 'ok', parts.join(' · '), summary.message)

  // 打开着的标签页得刷新，否则 TTL chip 显示的是陈旧读数。
  // 只刷读数，不动标签状态 —— 「写入即转正」的判据是"你对**它**动过手"，
  // 批量是对一个范围动手，把碰巧开着的预览页转正会莫名其妙（doc/frontend/tabs.md「写入即转正」）。
  sessions.invalidateKeyspace(sessionId)
}
</script>

<template>
  <Dlg
    v-if="target"
    v-model:open="open"
    title="批量设置过期时间"
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
            title="扫一遍但不改任何东西，只数出会影响多少个 Key"
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

      <div class="form-sec">
        <div class="cap">过期时间</div>
        <div class="pop-body" style="padding: 0">
          <TtlModePicker v-model="draft" />
        </div>
      </div>

      <div class="form-sec">
        <div class="cap">条件</div>

        <label class="pop-opt">
          <span
            class="radio"
            :class="{ on: condition === null }"
            @click="conditionsUsable && (condition = null)"
          />
          <span :class="{ muted: !conditionsUsable }">总是设置</span>
        </label>

        <label v-for="item in TTL_CONDITIONS" :key="item.value" class="pop-opt">
          <span
            class="radio"
            :class="{ on: condition === item.value }"
            @click="conditionsUsable && (condition = item.value)"
          />
          <span :class="{ muted: !conditionsUsable }">{{ item.label }}</span>
          <span class="spacer" />
          <span class="muted mono" style="font-size: var(--fs-10)">{{ item.flag }}</span>
        </label>

        <!-- 能力不足要在界面上说，不要等用户点下去才报错（与编解码器矩阵置灰未实现项同一条规矩，见 doc/backend/codec.md「归属切分」） -->
        <p v-if="conditionHint" class="field-hint" style="grid-column: auto; margin-top: 6px">
          {{ conditionHint }}
        </p>
        <p
          v-else-if="draft.mode === 'persist'"
          class="field-hint"
          style="grid-column: auto; margin-top: 6px"
        >
          PERSIST 没有条件参数
        </p>
      </div>
    </div>

    <template #footer>
      <div class="bulk-note">
        <!-- 过去的绝对时刻 = 立即删除。硬拦，并且把话说明白。 -->
        <template v-if="invalid">
          <span class="bulk-danger">
            绝对时刻必须晚于当前时间 —— 这等于立即删除{{ affectedLabel }}
          </span>
        </template>
        <template v-else-if="draft.mode === 'persist'">
          <span>{{ affectedLabel }}将不再过期</span>
        </template>
        <template v-else-if="resultAt">
          <span :class="{ 'bulk-danger': imminentIn !== null }">
            <Icon v-if="imminentIn !== null" name="alert-triangle" :size="12" />
            {{ affectedLabel }}将于 <b class="num">{{ formatDateTime(resultAt) }}</b> 过期
          </span>
        </template>
        <span class="bulk-cmd mono">{{ preview }}</span>
      </div>

      <span class="spacer" />
      <Btn :disabled="busy" @click="ui.closeBatchTtl()">取消</Btn>
      <Btn
        :variant="imminentIn !== null ? 'dangerSolid' : 'primary'"
        :disabled="!canSubmit"
        @click="submit"
      >
        {{ submitLabel }}
      </Btn>
    </template>
  </Dlg>
</template>
