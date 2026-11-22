<script setup lang="ts">
import { computed, ref } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Menu from '@/components/ui/Menu.vue'
import MenuItem from '@/components/ui/MenuItem.vue'
import { detectCodec, isImplemented, unimplementedReason } from '@/lib/codec'
import {
  chainLabel,
  extendBlockedReason,
  extendChain,
  resetChain,
  undoStep,
  type CodecChain,
} from '@/lib/codec/chain'
import { CODECS, codecLabel } from '@/lib/codecs'
import { useUiStore } from '@/stores/ui'
import type { CodecName, RedisText } from '@/types/ipc'

/**
 * 单 key 的临时编码覆盖（doc/backend/codec.md「三层配置」第 3 层），排查时用。
 *
 * **受控组件**：链的状态归调用方。保存时要沿链反向 encode，那份状态必须在
 * 有保存路径的那一侧；组件内部的状态也不会跟着 props 变，抽屉换行时会留着
 * 上一行的 codec 名。见 doc/frontend/codecs.md「链」。
 */
const props = defineProps<{
  /**
   * 这一步要解的字节。**不是原始值**，是链上当前这一层 ——
   * 解完 gzip 再点 msgpack，送过去的必须是解压产物。见 doc/frontend/codecs.md「链」。
   *
   * 收 `RedisText` 而不是 base64 字符串：截断守卫要看 `totalBytes`。
   */
  source: RedisText
  /** 已经应用的解码链。空链 = 还没解过。 */
  chain: CodecChain
}>()

/**
 * 产物是整条链而不是单次结果：调用方要拿它去反向 encode，也要靠它显示
 * `GZIP → MessagePack`。回退（撤销一步 / 还原原始值）走的也是这个事件。
 */
const emit = defineEmits<{ change: [chain: CodecChain] }>()

const ui = useUiStore()
const busy = ref(false)

/**
 * 为什么接不上下一步。两种：
 *
 *   - **值被截断** —— 半截 msgpack 跟半截 gzip 一样解不开，两类一起拦。
 *     压缩类不拦的话会让后端报 `codec.decode_failed`，那看起来像"这不是 gzip"，
 *     用户会以为自己认错了格式，而不是意识到值太大。见 doc/frontend/codecs.md「截断守卫」。
 *     下一步（"加载完整值"）由 `ValueDrawer` 的截断横幅给出。
 *   - **已经解出结构** —— 结构不是字节，套不了下一层。这时要留着回退入口，
 *     那是解错 codec 之后唯一的出路。
 *
 * 拦在菜单项上而不是点下去才报错，是 CLAUDE.md 的既有要求。
 */
const blockReason = computed(() => extendBlockedReason(props.chain, props.source))

const steps = computed(() => props.chain.steps)
const lastCodec = computed(() => steps.value[steps.value.length - 1]?.codec ?? null)
const label = computed(() => chainLabel(props.chain) || '编码')

function reasonFor(codec: CodecName): string | undefined {
  return blockReason.value ?? unimplementedReason(codec) ?? undefined
}

function trailFor(codec: CodecName): string | undefined {
  if (blockReason.value) return undefined
  return isImplemented(codec) ? undefined : '未实现'
}

async function decode(codec: CodecName) {
  if (blockReason.value) return
  busy.value = true
  try {
    const next = await extendChain(props.chain, codec, props.source)
    emit('change', next)
    ui.notify('ok', `已按 ${chainLabel(next)} 解码`)
  } catch (error) {
    ui.notifyError(error, `${codecLabel(codec)} 解码失败`)
  } finally {
    busy.value = false
  }
}

async function detect() {
  if (blockReason.value) return
  busy.value = true
  try {
    const codec = await detectCodec(props.source)
    if (!codec) {
      ui.notify('info', '没有识别出已支持的编码')
      return
    }
    await decode(codec)
  } catch (error) {
    ui.notifyError(error, '自动识别失败')
  } finally {
    busy.value = false
  }
}

/* 解错 codec 是常事。没有这两条，唯一的出路是把标签页关掉重开。 */
function undo() {
  emit('change', undoStep(props.chain))
}

function reset() {
  emit('change', resetChain(props.chain))
}
</script>

<template>
  <Menu align="start" :width="248">
    <template #trigger>
      <!-- 链条摆在按钮上：解了两层之后，"编码"两个字说不清现在看的是什么 -->
      <Btn size="sm" icon="braces" :disabled="busy">{{ label }}</Btn>
    </template>

    <!-- 接不上下一步时整张菜单的解码项都不可用，理由说在最上面，不用逐项重复 -->
    <div v-if="blockReason" class="menu-note">{{ blockReason }}</div>

    <MenuItem
      icon="wand"
      :disabled="Boolean(blockReason)"
      :title="blockReason ?? undefined"
      @select="detect"
    >
      自动识别
    </MenuItem>
    <div class="menu-sep" />
    <div class="menu-cap">编解码器</div>
    <MenuItem
      v-for="codec in CODECS"
      :key="codec.name"
      :disabled="Boolean(blockReason) || !isImplemented(codec.name)"
      :title="reasonFor(codec.name)"
      :trail="trailFor(codec.name)"
      @select="decode(codec.name)"
    >
      {{ codec.label }}
    </MenuItem>

    <!-- 回退**不受** blockReason 影响：解出结构之后能走的正是这两条 -->
    <template v-if="steps.length">
      <div class="menu-sep" />
      <MenuItem icon="chevron-left" @select="undo">
        撤销 {{ lastCodec ? codecLabel(lastCodec) : '' }}
      </MenuItem>
      <MenuItem v-if="steps.length > 1" icon="rotate-ccw" @select="reset">还原原始值</MenuItem>
    </template>
  </Menu>
</template>
