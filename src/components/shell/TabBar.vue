<script setup lang="ts">
import { computed } from 'vue'
import CtxMenu from '@/components/ui/CtxMenu.vue'
import Icon from '@/components/ui/Icon.vue'
import Menu from '@/components/ui/Menu.vue'
import MenuItem from '@/components/ui/MenuItem.vue'
import TypeDot from '@/components/ui/TypeDot.vue'
import { useSessionsStore } from '@/stores/sessions'
import { useTabsStore, type TabKind } from '@/stores/tabs'
import { useUiStore } from '@/stores/ui'
import type { IconName } from '@/lib/icons'

/**
 * 标签栏。`+` 下拉是全部非 key 标签页的**唯一主入口** ——
 * 见 doc/frontend/tabs.md「入口：标签栏的 `+` 菜单」。
 * 在此之前 CLI、Monitor、订阅这些面板在原型里都画了却没有来路。
 */
const tabs = useTabsStore()
const sessions = useSessionsStore()
const ui = useUiStore()

const KIND_ICON: Record<TabKind, IconName> = {
  key: 'key',
  browser: 'list-tree',
  cli: 'terminal',
  monitor: 'activity',
  pubsub: 'bell',
  memory: 'chart',
}

/**
 * 新标签页的作用域：侧栏当前选中节点向上找最近的 DB；
 * 没有选中时用最后一个活跃会话。防的是在 A 连接的上下文里开出连到 B 的 CLI ——
 * 这类误操作在只读/生产库场景下代价很高，事后还很难发现。
 */
const scopeSessionId = computed(
  () => sessions.selected?.sessionId ?? sessions.activeSessionId ?? null,
)
const scopeSession = computed(() =>
  scopeSessionId.value ? sessions.sessions.get(scopeSessionId.value) ?? null : null,
)
const scopeText = computed(() =>
  scopeSessionId.value ? tabs.scopeLabel(scopeSessionId.value) : '未连接',
)

function openTab(kind: Exclude<TabKind, 'key'>) {
  const sessionId = scopeSessionId.value
  if (!sessionId) {
    ui.notify('warn', '请先在左侧打开一个数据库')
    return
  }
  tabs.open({ kind, sessionId })
}
</script>

<template>
  <div class="tabbar">
    <div class="tabstrip">
      <CtxMenu v-for="tab in tabs.tabs" :key="tab.id" :width="204">
        <template #trigger>
          <div
            class="tab"
            :class="{ 'is-active': tab.id === tabs.activeId, 'is-preview': tab.preview }"
            :title="
              tab.kind === 'key'
                ? `${tab.key}\n${tabs.scopeLabel(tab.sessionId)}${tab.preview ? '\n预览页 · 双击固定' : ''}`
                : `${tabs.kindLabel(tab)} · ${tabs.scopeLabel(tab.sessionId)}`
            "
            @click="tabs.activate(tab.id)"
            @dblclick="tabs.promote(tab.id)"
            @mouseup.middle="tabs.close(tab.id)"
          >
            <TypeDot v-if="tab.kind === 'key'" :type="tab.keyType" />
            <Icon v-else :name="KIND_ICON[tab.kind]" :size="12" />

            <!-- key 标签显示 key 名；其余显示 `类型 · 作用域`。
                 截断时先压作用域、保住类型词：`CLI · analytics-se…` 可读，`Key 浏览…` 不可读。 -->
            <span v-if="tab.kind === 'key'" class="tab-name">{{ tab.key }}</span>
            <template v-else>
              <span class="tab-kind">{{ tabs.kindLabel(tab) }}</span>
              <span class="tab-scope">· {{ tabs.shortScope(tab.sessionId) }}</span>
            </template>

            <button class="tab-x" title="关闭标签页" @click.stop="tabs.close(tab.id)">
              <Icon name="x" :size="12" />
            </button>
          </div>
        </template>

        <MenuItem v-if="tab.preview" icon="plug" @select="tabs.promote(tab.id)">
          固定此标签页
        </MenuItem>
        <MenuItem icon="x" shortcut="Ctrl W" @select="tabs.close(tab.id)">关闭</MenuItem>
        <MenuItem icon="minus" @select="tabs.closeOthers(tab.id)">关闭其他</MenuItem>
        <MenuItem icon="chevron-right" @select="tabs.closeRight(tab.id)">关闭右侧</MenuItem>
        <div class="menu-sep" />
        <MenuItem icon="trash" danger @select="tabs.closeAll()">全部关闭</MenuItem>
      </CtxMenu>
    </div>

    <Menu align="start" :width="252">
      <template #trigger>
        <button class="btn btn-ghost btn-icon btn-sm tabadd" title="新建标签页">
          <Icon name="plus" :size="16" />
        </button>
      </template>

      <div class="menu-cap">新建标签页</div>
      <MenuItem icon="list-tree" shortcut="Ctrl B" @select="openTab('browser')">
        Key 浏览器
      </MenuItem>
      <MenuItem icon="terminal" shortcut="Ctrl `" @select="openTab('cli')">命令行</MenuItem>
      <MenuItem icon="activity" @select="openTab('monitor')">Monitor</MenuItem>
      <MenuItem icon="bell" @select="openTab('pubsub')">订阅 (Pub/Sub)</MenuItem>
      <div class="menu-sep" />
      <MenuItem icon="chart" @select="openTab('memory')">内存分析</MenuItem>
      <!-- 只读连接下只有导入需要置灰：浏览器、Monitor、订阅、内存分析都是读操作 -->
      <MenuItem
        icon="upload"
        :disabled="!!scopeSession?.readonly"
        :title="scopeSession?.readonly ? '该连接为只读模式，无法导入' : undefined"
        @select="ui.notify('info', '导入 / 导出请在 Key 树的右键菜单中选择目标范围')"
      >
        导入 / 导出…
      </MenuItem>

      <div class="menu-foot">
        <span>新标签页作用域</span>
        <span class="mono clip">{{ scopeText }}</span>
      </div>
    </Menu>

    <div class="tabtools">
      <button class="btn btn-ghost btn-icon btn-sm" title="分割视图将在后续版本提供" disabled>
        <Icon name="panel-left" :size="16" />
      </button>
    </div>
  </div>
</template>
