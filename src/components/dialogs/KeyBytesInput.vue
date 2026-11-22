<script setup lang="ts">
import { ref } from 'vue'
import { encodeDraft, type ByteDraft } from '@/lib/keyCreation'
import { bytesFromBase64 } from '@/lib/redisText'

/**
 * 一格「字节」输入：textarea + 右上角一枚格式 chip（UTF-8 / Base64）。
 *
 * 新建 Key 的每个字段、成员、值都可以各自选格式（doc/frontend/key-write-dialog.md「名称与字节」），
 * 所以格式开关必须长在格子里，而不是整张表单一个。做成 chip 而不是下拉，
 * 是为了让它塞进 28px 的行高里 —— 一行 hash 就是「字段 + 值」两格，
 * 每格再挂一个下拉，行就散了。
 */
const props = withDefaults(
  defineProps<{
    /** 无障碍名称，例如「字段 1」 */
    label: string
    placeholder?: string
    rows?: number
  }>(),
  { rows: 1 },
)
const draft = defineModel<ByteDraft>({ required: true })
const error = ref('')

/** 切换格式时转换原字节；非 UTF-8 不允许转成有损文本。 */
function toggleEncoding() {
  const encoding = draft.value.encoding === 'text' ? 'base64' : 'text'
  try {
    const encoded = encodeDraft(draft.value)
    const text =
      encoding === 'base64'
        ? encoded
        : new TextDecoder('utf-8', { fatal: true }).decode(bytesFromBase64(encoded))
    draft.value = { encoding, text }
    error.value = ''
  } catch {
    error.value = '内容无法转换，请检查 Base64；非 UTF-8 字节请保留 Base64 格式'
  }
}
</script>

<template>
  <div class="keywrite-bytes">
    <div class="input-wrap">
      <textarea
        v-model="draft.text"
        class="input mono"
        :rows="rows"
        :placeholder="placeholder"
        :aria-label="props.label"
        spellcheck="false"
        @input="error = ''"
      />
      <span class="trail">
        <button
          type="button"
          class="chipbtn"
          :aria-label="`${props.label}输入格式`"
          :title="draft.encoding === 'text' ? '按 UTF-8 文本输入，点击改为 Base64' : '按 Base64 输入，点击改为 UTF-8 文本'"
          @click="toggleEncoding"
        >
          {{ draft.encoding === 'text' ? 'UTF-8' : 'Base64' }}
        </button>
      </span>
    </div>
    <p v-if="error" class="field-err" role="alert">{{ error }}</p>
  </div>
</template>
