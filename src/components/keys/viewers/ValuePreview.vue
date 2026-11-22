<script setup lang="ts">
import { computed } from 'vue'
import { detectShape } from '@/lib/redisValue'
import { isBinaryText, isTruncated, textOf } from '@/lib/redisText'
import { toSingleLine } from '@/lib/format'
import type { RedisText } from '@/types/ipc'

/**
 * 表格里的单行值预览。
 *
 * `.v` 必须是等宽 —— 十六进制掉字体等于白给，字节完全对不齐。
 * `.vtag` 故意保持界面字体：它是标注不是数据。
 */
const props = defineProps<{ value: RedisText }>()

/**
 * 形态判定吃**未截断**的文本，只有显示才走 `toSingleLine`。
 *
 * 两者共用一份截断过的文本时，`toSingleLine` 的 400 字符上限会在尾部接一个
 * `…`，而 `detectShape` 先看首尾字符——尾字符不是 `}` 就直接判 text，连 parse
 * 都不会试。结果正好反了：超过 400 字符的 JSON（最需要标签的那些）全都拿不到
 * 标签，而抽屉用的是完整文本，同一个值在两处结论相反。
 *
 * 开销可控：集合成员与 Hash 字段在后端就按 64 KiB 截断（`MEMBER_VALUE_LIMIT`），
 * 与 `detectShape` 的探测上限同值，所以最坏是每个**可见**行一次 64 KiB 的
 * parse——列表是虚拟化的，没实例化的行不会求值。
 */
const full = computed(() => textOf(props.value))
const text = computed(() => toSingleLine(full.value))

const tag = computed(() => {
  // 二进制先判：非 UTF-8 的字节渲染出来是十六进制，再去 detectShape 没有意义
  if (isBinaryText(props.value)) return 'BIN'
  return detectShape(full.value) === 'json' ? 'JSON' : null
})

/* 截断必须显式告诉用户，不能默默给一个看起来完整的值。 */
const truncated = computed(() => isTruncated(props.value))
</script>

<template>
  <span class="vprev">
    <span v-if="tag" class="vtag">{{ tag }}</span>
    <span class="v">{{ text }}</span>
    <span v-if="truncated" class="vtag" title="值过大，只取回了前一段">截断</span>
  </span>
</template>
