<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import Tip from '@/components/ui/Tip.vue'
import Menu from '@/components/ui/Menu.vue'
import MenuItem from '@/components/ui/MenuItem.vue'
import TypeChip from '@/components/ui/TypeChip.vue'
import TtlChip from './TtlChip.vue'
import StringViewer from './viewers/StringViewer.vue'
import HllViewer from './viewers/HllViewer.vue'
import HashViewer from './viewers/HashViewer.vue'
import ListViewer from './viewers/ListViewer.vue'
import SetViewer from './viewers/SetViewer.vue'
import ZSetViewer from './viewers/ZSetViewer.vue'
import StreamViewer from './viewers/StreamViewer.vue'
import { useKeyMeta } from './useKeyMeta'
import { deleteKeys, renameKey } from '@/api/redis'
import { formatBytes, formatCount } from '@/lib/format'
import { keyTypeMeta } from '@/lib/keyTypes'
import { useSessionsStore } from '@/stores/sessions'
import { useSettingsStore } from '@/stores/settings'
import { useTabsStore } from '@/stores/tabs'
import { useUiStore } from '@/stores/ui'
import type { Tab } from '@/stores/tabs'

const props = defineProps<{ tab: Tab }>()

const sessions = useSessionsStore()
const settings = useSettingsStore()
const tabs = useTabsStore()
const ui = useUiStore()

const keyName = computed(() => props.tab.key ?? '')
const session = computed(() => sessions.sessions.get(props.tab.sessionId))
const readonly = computed(() => !!session.value?.readonly)

/* 是不是集群由后端自己判断 —— 它比前端更清楚这条连接的形态 */
const meta = useKeyMeta(props.tab.sessionId, keyName.value)

/** 视图自己的重载版本号：改完值让子组件整体重取，而不是各自维护脏状态。 */
const revision = ref(0)

onMounted(async () => {
  await meta.refresh()
  // 标签页上的类型色点在打开时可能还是空的，元信息回来后补上
  if (meta.type.value) props.tab.keyType = meta.type.value
})

async function reload() {
  await meta.refresh()
  revision.value += 1
}

/**
 * 批量操作改的是一个范围，可能正好覆盖了这个 key —— 重取读数，
 * 否则 TTL chip 会一直显示批量执行之前的值。
 * 不调 `onChanged`：那会把预览页转正，而这次写入不是冲着这个 key 来的。
 */
watch(
  () => sessions.revisionOf(props.tab.sessionId),
  () => {
    void reload()
  },
)

/**
 * 写入成功后把预览页转正。
 *
 * 判据是"你对这个 key 动过手"：既然改了值、改了过期时间、重命名了，
 * 多半还要回来看它，不该被下一次单击顶掉。刷新按钮不算 —— 那只是看看。
 * 各视图的 `changed` 只在写成功后才发，所以这里等价于"写入成功"。
 */
async function onChanged() {
  tabs.promote(props.tab.id)
  await reload()
}

const scope = computed(() => tabs.scopeLabel(props.tab.sessionId))
const unit = computed(() => keyTypeMeta(meta.type.value).unit)

async function copyKey() {
  try {
    await navigator.clipboard.writeText(keyName.value)
    ui.notify('ok', '已复制 Key 名')
  } catch (error) {
    ui.notifyError(error, '复制失败')
  }
}

const renaming = ref(false)
const newName = ref('')

function startRename() {
  newName.value = keyName.value
  renaming.value = true
}

async function commitRename() {
  const target = newName.value.trim()
  if (!target || target === keyName.value) {
    renaming.value = false
    return
  }
  try {
    await renameKey(props.tab.sessionId, keyName.value, target)
    sessions.forgetKey(props.tab.sessionId, keyName.value)
    sessions.rememberKey(props.tab.sessionId, target, meta.type.value)
    props.tab.key = target
    renaming.value = false
    ui.notify('ok', '已重命名')
    await onChanged()
  } catch (error) {
    ui.notifyError(error, '重命名失败')
  }
}

async function remove() {
  if (settings.state.confirmDelete) {
    const ok = await ui.confirm({
      title: '删除 Key',
      body: '删除后无法恢复。',
      detail: keyName.value,
      confirmText: '删除',
      danger: true,
    })
    if (!ok) return
  }
  try {
    await deleteKeys(props.tab.sessionId, [keyName.value])
    sessions.forgetKey(props.tab.sessionId, keyName.value)
    tabs.close(props.tab.id)
    ui.notify('ok', '已删除')
  } catch (error) {
    ui.notifyError(error, '删除失败')
  }
}
</script>

<template>
  <template v-if="meta.missing.value">
    <!-- TTL 归零或 key 被删：切成失效状态，不要继续显示一张陈旧的表 -->
    <div class="emptystate">
      <Icon name="clock" :size="20" />
      <h3>该 Key 已不存在</h3>
      <p class="mono selectable">{{ keyName }}</p>
      <p>它可能已经过期，或者被其他客户端删除了。</p>
      <div style="margin-top: 16px; display: flex; gap: 8px">
        <Btn icon="refresh" @click="reload">重新检查</Btn>
        <Btn @click="tabs.close(tab.id)">关闭标签页</Btn>
      </div>
    </div>
  </template>

  <template v-else>
    <div class="keyhdr">
      <div class="kh-main grow">
        <h2>
          <template v-if="renaming">
            <input
              v-model="newName"
              class="input mono"
              style="width: 320px"
              autofocus
              @keydown.enter="commitRename"
              @keydown.esc="renaming = false"
              @blur="renaming = false"
            />
          </template>
          <span v-else class="kname">{{ keyName }}</span>
          <TypeChip :type="meta.type.value" />
          <!-- HLL 是 string 的子形态：类型徽标保持 STRING（类型色三处出现的规则
               不新增颜色），子形态用中性 tag 说明 -->
          <span v-if="meta.hll.value" class="tag">HyperLogLog</span>
        </h2>

        <!-- TTL 已经提升为右侧的控件，这一行不再重复它 -->
        <div class="keymeta">
          <span v-if="meta.size.value !== null">
            <b>{{ formatCount(meta.size.value) }}</b> {{ unit }}
          </span>
          <span v-if="meta.hll.value">HLL <b>{{ meta.hll.value.encoding }}</b></span>
          <span v-if="meta.memory.value !== null"><b>{{ formatBytes(meta.memory.value) }}</b></span>
          <span v-if="meta.encoding.value">编码 <b>{{ meta.encoding.value }}</b></span>
          <span v-if="meta.slot.value !== null">slot <b>{{ meta.slot.value }}</b></span>
          <span>{{ scope }}</span>
        </div>
      </div>

      <div class="kh-act">
        <TtlChip
          :session-id="tab.sessionId"
          :key-name="keyName"
          :ttl="meta.ttl.value"
          :readonly="readonly"
          @changed="meta.ttl.value = $event; tabs.promote(tab.id)"
        />
        <span class="vsep" />
        <Tip text="刷新">
          <Btn variant="ghost" icon="refresh" icon-only @click="reload" />
        </Tip>
        <Tip text="重命名">
          <Btn variant="ghost" icon="pencil" icon-only :disabled="readonly" @click="startRename" />
        </Tip>
        <Tip text="复制 Key 名">
          <Btn variant="ghost" icon="copy" icon-only @click="copyKey" />
        </Tip>
        <Menu align="end">
          <template #trigger><Btn variant="ghost" icon="more" icon-only title="更多操作" /></template>
          <MenuItem icon="copy" @select="ui.openCloneKey(tab.sessionId, keyName)">克隆 Key…</MenuItem>
        </Menu>
        <span class="vsep" />
        <Tip :text="readonly ? '该连接为只读模式' : '删除'">
          <Btn variant="ghost" icon="trash" icon-only :disabled="readonly" @click="remove" />
        </Tip>
      </div>
    </div>

    <!-- HLL 在 StringViewer 之前分发：把一个 HLL 交给字符串编辑器，
         等于把"裸编辑保存即损坏"的路径重新打开 -->
    <HllViewer
      v-if="meta.type.value === 'string' && meta.hll.value"
      :key="`hll-${revision}`"
      :session-id="tab.sessionId"
      :key-name="keyName"
      :readonly="readonly"
      @changed="onChanged"
    />
    <StringViewer
      v-else-if="meta.type.value === 'string'"
      :key="`string-${revision}`"
      :session-id="tab.sessionId"
      :key-name="keyName"
      :readonly="readonly"
      @changed="onChanged"
    />
    <HashViewer
      v-else-if="meta.type.value === 'hash'"
      :key="`hash-${revision}`"
      :session-id="tab.sessionId"
      :key-name="keyName"
      :readonly="readonly"
      @changed="onChanged"
    />
    <ListViewer
      v-else-if="meta.type.value === 'list'"
      :key="`list-${revision}`"
      :session-id="tab.sessionId"
      :key-name="keyName"
      :total="meta.size.value"
      :readonly="readonly"
      @changed="onChanged"
    />
    <SetViewer
      v-else-if="meta.type.value === 'set'"
      :key="`set-${revision}`"
      :session-id="tab.sessionId"
      :key-name="keyName"
      :readonly="readonly"
      @changed="onChanged"
    />
    <ZSetViewer
      v-else-if="meta.type.value === 'zset'"
      :key="`zset-${revision}`"
      :session-id="tab.sessionId"
      :key-name="keyName"
      :total="meta.size.value"
      :readonly="readonly"
      @changed="onChanged"
    />
    <StreamViewer
      v-else-if="meta.type.value === 'stream'"
      :key="`stream-${revision}`"
      :session-id="tab.sessionId"
      :key-name="keyName"
      :readonly="readonly"
    />
    <div v-else class="emptyline">
      <Icon name="loader" :size="16" />
      <span>{{ meta.loading.value ? '正在读取…' : meta.error.value?.text ?? '未知类型' }}</span>
    </div>
  </template>
</template>
