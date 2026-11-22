<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Icon from '@/components/ui/Icon.vue'
import Pop from '@/components/ui/Pop.vue'
import Seg from '@/components/ui/Seg.vue'
import Sw from '@/components/ui/Sw.vue'
import Tip from '@/components/ui/Tip.vue'
import KeyTree from './KeyTree.vue'
import { deleteKeys } from '@/api/redis'
import { useConnectionsStore } from '@/stores/connections'
import { useSessionsStore } from '@/stores/sessions'
import { useSettingsStore } from '@/stores/settings'
import { useTabsStore } from '@/stores/tabs'
import { useUiStore } from '@/stores/ui'

const connections = useConnectionsStore()
const sessions = useSessionsStore()
const settings = useSettingsStore()
const tabs = useTabsStore()
const ui = useUiStore()

const search = ref('')
const active = computed(() => sessions.activeSession)
/** 前缀作用域 chip（doc/frontend/key-tree.md「文件夹内搜索与前缀权威」）：直接读会话状态，右键「在此搜索」设置、× 清除。 */
const scope = computed(() => active.value?.searchScope ?? null)

/* 搜索走「扫到底」的 search_keys 任务（doc/frontend/key-tree.md「搜索」）：非空输入启动
   任务、key 流式并入树；清空输入的分流在 store 里 —— 有前缀作用域就是
   「扫描此前缀」的搜索态，没有才回浏览态重扫。`*词*` 包裹与精确命中探测
   也都在 store 里。防抖 260ms —— 再短会在连打时反复起任务又取消。 */
let searchTimer: number | undefined
/** 程序化回填搜索框（切会话恢复词）不触发任务 —— 只有用户输入才触发。 */
let restoring = false
watch(search, (value) => {
  if (restoring) {
    restoring = false
    return
  }
  window.clearTimeout(searchTimer)
  const session = active.value
  if (!session) return
  searchTimer = window.setTimeout(() => {
    void sessions.startSearch(session.id, value)
  }, 260)
})

/** 切会话：取消上一个会话进行中的搜索任务（doc/frontend/key-tree.md「取消时机」之一 ——
    看不见的会话不该继续烧服务端），再把搜索框恢复成新会话自己的词。
    恢复的是原词而不是拼好的 pattern：作用域 chip 从会话状态直接渲染，
    回填整个 pattern 会把前缀重复一遍。 */
watch(
  () => active.value?.id,
  (_now, prev) => {
    window.clearTimeout(searchTimer)
    if (prev) sessions.cancelSearch(prev)
    const restored = active.value?.searchQuery ?? ''
    if (restored !== search.value) {
      restoring = true
      search.value = restored
    }
  },
)

/** 清 chip 之后怎么走（词还在→全局搜；词空→回浏览态）由 store 定夺。 */
function clearScope() {
  if (active.value) void sessions.clearSearchScope(active.value.id)
}

/** 空输入框里按 Backspace = 摘掉前缀 chip，与常见 token 输入框的习惯一致。 */
function onSearchKeydown(event: KeyboardEvent) {
  if (event.key === 'Backspace' && !search.value && scope.value) clearScope()
}

/* 窗口隐藏时暂停搜索任务、可见时恢复（doc/frontend/key-tree.md「取消时机」）。暂停不是取消：
   后端在页间等待，恢复后从当前游标继续。 */
function onVisibilityChange() {
  sessions.setSearchTasksPaused(document.hidden)
}
onMounted(() => document.addEventListener('visibilitychange', onVisibilityChange))
onBeforeUnmount(() => document.removeEventListener('visibilitychange', onVisibilityChange))

const modeOptions = [
  { value: 'tree', label: '树形', icon: 'list-tree' as const },
  { value: 'list', label: '列表', icon: 'list' as const },
]

async function onDeleteKey(sessionId: string, key: string) {
  const session = sessions.sessions.get(sessionId)
  if (session?.readonly) {
    ui.notify('warn', '该连接为只读模式，无法删除')
    return
  }
  if (settings.state.confirmDelete) {
    const ok = await ui.confirm({
      title: '删除 Key',
      body: '删除后无法恢复。',
      detail: key,
      confirmText: '删除',
      danger: true,
    })
    if (!ok) return
  }
  try {
    await deleteKeys(sessionId, [key])
    sessions.forgetKey(sessionId, key)
    const tab = tabs.findKeyTab(sessionId, key)
    if (tab) tabs.close(tab.id)
    ui.notify('ok', '已删除')
  } catch (error) {
    ui.notifyError(error, '删除失败')
  }
}
</script>

<template>
  <aside class="side">
    <div class="side-hd">
      <span class="t">连接</span>
      <span class="spacer" />
      <Tip text="新建连接">
        <Btn variant="ghost" size="sm" icon="plus" icon-only @click="ui.openConnectionDialog()" />
      </Tip>
      <Tip text="重新读取连接列表">
        <Btn variant="ghost" size="sm" icon="refresh" icon-only @click="connections.refresh()" />
      </Tip>
    </div>

    <div class="side-search">
      <div class="input-wrap">
        <span class="lead"><Icon name="search" :size="12" /></span>
        <!-- 前缀作用域 chip：前缀是 key 路径的一部分 → 等宽字体；
             作用域是过滤器不是危险物 → accent 一族，红色只留给危险。 -->
        <span v-if="scope" class="side-scope" :title="`${scope}${active?.separator ?? ':'}`">
          <span class="t">{{ scope }}{{ active?.separator ?? ':' }}</span>
          <button class="x" type="button" title="清除前缀作用域" @click="clearScope">
            <Icon name="x" :size="12" />
          </button>
        </span>
        <input
          v-model="search"
          class="input"
          type="search"
          :placeholder="active ? (scope ? '在此前缀下搜索' : `在 db${active.db} 中搜索 Key`) : '先打开一个数据库'"
          :disabled="!active"
          @keydown="onSearchKeydown"
        />
      </div>

      <Pop :width="264" align="end">
        <template #trigger>
          <Btn variant="ghost" icon="filter" icon-only title="扫描选项" />
        </template>
        <template #default>
          <div class="pop-hd">扫描选项</div>
          <div class="pop-body">
            <div class="field" style="grid-template-columns: 84px minmax(0, 1fr)">
              <label>SCAN COUNT</label>
              <div class="field-ctl">
                <input v-model.number="settings.state.scanCount" class="input w-88" type="number" min="10" step="10" />
                <span class="muted" style="font-size: var(--fs-11)">每批</span>
              </div>
            </div>
            <div class="field" style="grid-template-columns: 84px minmax(0, 1fr)">
              <label>树节点上限</label>
              <div class="field-ctl">
                <input v-model.number="settings.state.treeNodeLimit" class="input w-88" type="number" min="1000" step="1000" />
              </div>
            </div>
            <div class="pop-opt">
              <Sw v-model="settings.state.indentGuides" size="sm" />
              <span>缩进参考线</span>
            </div>
            <div class="pop-opt" :title="'每个 key 多一条 TTL 命令，与 TYPE 同批 pipeline'">
              <Sw v-model="settings.state.treeTtl" size="sm" />
              <span>在树里显示 TTL</span>
            </div>
          </div>
          <div class="pop-note">
            改动会在下次扫描时生效。
            <span class="cmd">SCAN cursor MATCH {{ active?.pattern || '*' }} COUNT {{ settings.state.scanCount }}</span>
          </div>
        </template>
      </Pop>
    </div>

    <div class="side-modes">
      <Seg v-model="settings.state.defaultTreeMode" :options="modeOptions" />
      <!-- 紧凑开关放在这里而不是漏斗弹层里：它的效果需要"开一下关一下对比"
           才能理解，藏进弹层会让对比成本变高。 -->
      <Tip text="紧凑路径：把只有一个子节点的层级合并成一行">
        <Btn
          variant="ghost"
          size="sm"
          icon="arrow-down-up"
          icon-only
          :active="settings.state.compactPaths"
          :disabled="settings.state.defaultTreeMode === 'list'"
          @click="settings.state.compactPaths = !settings.state.compactPaths"
        />
      </Tip>
      <span class="spacer" />
      <Tip text="重新扫描当前数据库">
        <Btn
          variant="ghost"
          size="sm"
          icon="refresh"
          icon-only
          :disabled="!active"
          @click="active && sessions.rescan(active.id)"
        />
      </Tip>
    </div>

    <KeyTree @delete-key="onDeleteKey" />

    <div class="side-ft">
      <Btn variant="ghost" size="sm" icon="plus" @click="ui.openConnectionDialog()">新建连接</Btn>
      <span class="spacer" />
      <Tip text="设置">
        <Btn variant="ghost" size="sm" icon="settings" icon-only @click="ui.openSettings()" />
      </Tip>
    </div>
  </aside>
</template>
