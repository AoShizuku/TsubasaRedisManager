<script setup lang="ts">
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import { formatCount } from '@/lib/format'

/** 集合类视图共用的工具条：值内搜索、加载进度、加载更多、新增。 */
withDefaults(
  defineProps<{
    loaded: number
    total?: number | null
    complete: boolean
    loading: boolean
    readonly: boolean
    /** List 没有服务端的成员过滤命令，那里就别摆一个搜不了的框 */
    searchable?: boolean
    searchPlaceholder?: string
    addLabel: string
  }>(),
  { searchable: true, searchPlaceholder: '搜索' },
)

const emit = defineEmits<{ search: [value: string]; loadMore: []; add: [] }>()
const search = defineModel<string>('search', { default: '' })

let timer: number | undefined
function onInput() {
  window.clearTimeout(timer)
  timer = window.setTimeout(() => emit('search', search.value.trim()), 260)
}
</script>

<template>
  <div class="toolbar">
    <div v-if="searchable" class="input-wrap" style="max-width: 280px">
      <span class="lead"><Icon name="search" :size="12" /></span>
      <!-- 值内搜索走 SCAN 的 MATCH，和侧栏的全局搜索是两回事 -->
      <input
        v-model="search"
        class="input"
        type="search"
        :placeholder="searchPlaceholder"
        @input="onInput"
      />
    </div>

    <span class="muted num" style="font-size: var(--fs-11)">
      已加载 {{ formatCount(loaded) }}<template v-if="total != null"> / {{ formatCount(total) }}</template>
    </span>

    <Btn
      v-if="!complete"
      size="sm"
      :icon="loading ? 'loader' : 'chevron-down'"
      :disabled="loading"
      @click="emit('loadMore')"
    >
      {{ loading ? '加载中…' : '加载更多' }}
    </Btn>

    <span class="spacer" />
    <Btn
      size="sm"
      icon="plus"
      :disabled="readonly"
      :title="readonly ? '该连接为只读模式' : undefined"
      @click="emit('add')"
    >
      {{ addLabel }}
    </Btn>
  </div>
</template>
