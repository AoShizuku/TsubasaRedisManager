<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Dlg from '@/components/ui/Dlg.vue'
import Icon from '@/components/ui/Icon.vue'
import Seg from '@/components/ui/Seg.vue'
import Sw from '@/components/ui/Sw.vue'
import { checkUpdate, getFonts, openExternal, openLicenses, runFormatter } from '@/api/native'
import { isImplemented } from '@/lib/codec'
import { CODECS, CONCLUSION_LABEL } from '@/lib/codecs'
import { base64FromText } from '@/lib/redisValue'
import { useConnectionsStore } from '@/stores/connections'
import { useSettingsStore } from '@/stores/settings'
import { useUiStore, type SettingsSection } from '@/stores/ui'
import type { IconName } from '@/lib/icons'
import type { UpdateCheckResult } from '@/types/ipc'

const settings = useSettingsStore()
const connections = useConnectionsStore()
const ui = useUiStore()

const SECTIONS: Array<{ key: SettingsSection; label: string; icon: IconName }> = [
  { key: 'general', label: '通用', icon: 'sliders' },
  { key: 'appearance', label: '外观', icon: 'palette' },
  { key: 'editor', label: '编辑器', icon: 'file-text' },
  { key: 'security', label: '连接与安全', icon: 'shield' },
  { key: 'viewer', label: '数据查看器', icon: 'braces' },
  { key: 'formatter', label: '格式化器', icon: 'wand' },
  { key: 'shortcuts', label: '快捷键', icon: 'keyboard' },
  { key: 'about', label: '关于', icon: 'info' },
]

const open = computed({
  get: () => ui.settingsDialog.open,
  set: (value: boolean) => {
    if (!value) ui.closeSettings()
  },
})

const section = computed({
  get: () => ui.settingsDialog.section,
  set: (value: SettingsSection) => {
    ui.settingsDialog = { ...ui.settingsDialog, section: value }
  },
})

/* vite.config.ts 从 tauri.conf.json 注入 */
const version = __APP_VERSION__

/**
 * 「关于」里列出的主要第三方组件。
 *
 * 完整清单有 654 项，在随程序分发的 THIRD_PARTY_LICENSES.txt 里（「查看全文」
 * 打开的就是它）。这里只挑用户可能想知道的：界面框架、编辑器、协议实现。
 * 许可证标识符照抄各自 manifest 里声明的，别凭印象写 —— redis-rs 是
 * BSD-3-Clause 不是 MIT，这正是清单曾经写错的地方。
 */
const CREDITS = [
  { name: 'Vue', license: 'MIT' },
  { name: 'Tauri', license: 'Apache-2.0 OR MIT' },
  { name: 'Monaco Editor', license: 'MIT' },
  { name: 'reka-ui', license: 'MIT' },
  { name: 'Lucide', license: 'ISC' },
  { name: 'redis-rs', license: 'BSD-3-Clause' },
  { name: 'russh', license: 'Apache-2.0' },
  { name: 'rustls', license: 'Apache-2.0 OR ISC OR MIT' },
]

async function showLicenses() {
  try {
    await openLicenses()
  } catch (error) {
    ui.notifyError(error, '打开第三方许可文件失败')
  }
}

const densityOptions = [
  { value: 'compact', label: '紧凑' },
  { value: 'standard', label: '标准' },
  { value: 'cozy', label: '宽松' },
]

const treeModeOptions = [
  { value: 'tree', label: '树形' },
  { value: 'list', label: '列表' },
]

/* 字体候选来自系统枚举，第一次打开外观分组时才去取 —— 这是个不便宜的调用 */
watch(
  () => [open.value, section.value] as const,
  async ([isOpen, current]) => {
    if (!isOpen || current !== 'appearance' || settings.fontCandidates.length) return
    try {
      settings.fontCandidates = await getFonts()
    } catch {
      settings.fontCandidates = []
    }
  },
)

/* ── 格式化器试运行 ── */
const formatterSample = ref('{"hello":"world"}')
const formatterResult = ref<{ ok: boolean; text: string; preview: string } | null>(null)
const formatterBusy = ref(false)

async function testFormatter() {
  formatterBusy.value = true
  formatterResult.value = null
  try {
    const result = await runFormatter({
      command: settings.state.formatterCommand,
      params: settings.state.formatterParams || null,
      valueBase64: base64FromText(formatterSample.value),
      timeoutMs: settings.state.formatterTimeoutMs,
      maxOutputBytes: settings.state.formatterMaxOutputBytes,
    })
    formatterResult.value = {
      ok: !result.timedOut && (result.statusCode ?? 0) === 0,
      text: result.stdout || result.stderr || '(无输出)',
      preview: result.previewCommand,
    }
  } catch (error) {
    const fault = ui.notifyError(error, '格式化器运行失败')
    formatterResult.value = { ok: false, text: fault.text, preview: '' }
  } finally {
    formatterBusy.value = false
  }
}

/* ── 更新检查 ── */
const update = ref<UpdateCheckResult | null>(null)
const updateBusy = ref(false)

async function doCheckUpdate() {
  updateBusy.value = true
  try {
    update.value = await checkUpdate({
      endpoint: settings.state.updateEndpoint || null,
      currentVersion: null,
    })
  } catch (error) {
    ui.notifyError(error, '检查更新失败')
  } finally {
    updateBusy.value = false
  }
}

async function resetAll() {
  const ok = await ui.confirm({
    title: '恢复默认设置',
    body: '所有界面偏好会恢复到初始值。已保存的连接不受影响。',
    confirmText: '恢复默认',
    danger: true,
  })
  if (ok) {
    settings.reset()
    ui.notify('ok', '已恢复默认设置')
  }
}
</script>

<template>
  <Dlg v-model:open="open" title="设置" :width="860">
    <div class="dlg-split" style="height: 560px">
      <nav class="set-nav">
        <button
          v-for="item in SECTIONS"
          :key="item.key"
          class="set-nav-item"
          :class="{ 'is-active': section === item.key }"
          @click="section = item.key"
        >
          <Icon :name="item.icon" :size="16" />
          {{ item.label }}
        </button>
      </nav>

      <div class="set-body">
        <!-- ── 通用 ── -->
        <template v-if="section === 'general'">
          <h3>通用</h3>
          <p class="lead">扫描行为与删除确认。</p>
          <div class="set-group">
            <div class="srow">
              <div class="srow-txt">
                <b>SCAN COUNT</b>
                <p>每批扫描的提示值。调大能减少往返次数，但单次阻塞时间更长。</p>
              </div>
              <div class="srow-ctl">
                <input v-model.number="settings.state.scanCount" class="input w-120" type="number" min="10" step="10" />
              </div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>任务 SCAN COUNT</b>
                <p>搜索与前缀扫描这类扫到底任务的每批提示值。命中稀疏时回包小，调大能省往返。</p>
              </div>
              <div class="srow-ctl">
                <input v-model.number="settings.state.taskScanCount" class="input w-120" type="number" min="100" step="100" />
              </div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>树节点上限</b>
                <p>已加载 Key 超过这个数就停止自动续扫，改为显示「加载更多」。</p>
              </div>
              <div class="srow-ctl">
                <input v-model.number="settings.state.treeNodeLimit" class="input w-120" type="number" min="1000" step="1000" />
              </div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>删除前确认</b>
                <p>关闭后，删除 Key 与集合成员将不再弹出确认。</p>
              </div>
              <div class="srow-ctl"><Sw v-model="settings.state.confirmDelete" /></div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>即将过期阈值</b>
                <p>TTL 低于这个秒数时，标签变成琥珀色。</p>
              </div>
              <div class="srow-ctl">
                <input v-model.number="settings.state.ttlSoonThreshold" class="input w-88" type="number" min="1" />
                <span class="muted" style="font-size: var(--fs-11)">秒</span>
              </div>
            </div>
          </div>
        </template>

        <!-- ── 外观 ── -->
        <template v-else-if="section === 'appearance'">
          <h3>外观</h3>
          <p class="lead">密度、字体与缩放。</p>
          <div class="set-group">
            <div class="srow is-disabled">
              <div class="srow-txt">
                <b>深色主题</b>
                <p>v0.1 只提供浅色主题。</p>
              </div>
              <div class="srow-ctl"><Sw :model-value="false" disabled /></div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>行高密度</b>
                <p>影响 Key 树与数据表的行高。</p>
              </div>
              <div class="srow-ctl"><Seg v-model="settings.state.density" :options="densityOptions" /></div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>界面字体</b>
                <p>用于按钮、菜单、说明文字这类标注性内容。</p>
              </div>
              <div class="srow-ctl">
                <div class="select">
                  <select v-model="settings.state.uiFont" class="input w-160">
                    <option value="">默认</option>
                    <option v-for="font in settings.fontCandidates" :key="font" :value="font">{{ font }}</option>
                  </select>
                  <span class="caret"><Icon name="chevron-down" :size="12" /></span>
                </div>
              </div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>等宽字体</b>
                <p>用于 Key 名、字段、值、Stream ID 这类数据。</p>
              </div>
              <div class="srow-ctl">
                <div class="select">
                  <select v-model="settings.state.monoFont" class="input w-160">
                    <option value="">默认</option>
                    <option v-for="font in settings.fontCandidates" :key="font" :value="font">{{ font }}</option>
                  </select>
                  <span class="caret"><Icon name="chevron-down" :size="12" /></span>
                </div>
              </div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>界面缩放</b>
                <p>当前 {{ Math.round(settings.state.zoom * 100) }}%</p>
              </div>
              <div class="srow-ctl">
                <Btn size="sm" icon="minus" icon-only @click="settings.setZoom(settings.state.zoom - 0.1)" />
                <Btn size="sm" @click="settings.setZoom(1)">100%</Btn>
                <Btn size="sm" icon="plus" icon-only @click="settings.setZoom(settings.state.zoom + 0.1)" />
              </div>
            </div>
          </div>

          <div class="set-group">
            <div class="cap">Key 树</div>
            <div class="srow">
              <div class="srow-txt">
                <b>默认视图</b>
                <p>列表模式去掉缩进，只显示 Key 名与类型点。</p>
              </div>
              <div class="srow-ctl"><Seg v-model="settings.state.defaultTreeMode" :options="treeModeOptions" /></div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>紧凑路径</b>
                <p>把只有一个子节点的层级合并成一行，直到第一个有多个子节点的层级为止。</p>
              </div>
              <div class="srow-ctl"><Sw v-model="settings.state.compactPaths" /></div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>缩进参考线</b>
                <p>深度 ≥ 3 时靠它判断从属关系。</p>
              </div>
              <div class="srow-ctl"><Sw v-model="settings.state.indentGuides" /></div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>显示 TTL 标记</b>
                <p>每个 key 多一条 TTL 命令，与 TYPE 同批 pipeline。快过期的会标成琥珀色。</p>
              </div>
              <div class="srow-ctl"><Sw v-model="settings.state.treeTtl" /></div>
            </div>
          </div>
        </template>

        <!-- ── 编辑器 ── -->
        <template v-else-if="section === 'editor'">
          <h3>编辑器</h3>
          <p class="lead">STRING 值编辑器的行为。</p>
          <div class="set-group">
            <div class="srow">
              <div class="srow-txt"><b>自动换行</b><p>长行折回，不出现横向滚动。</p></div>
              <div class="srow-ctl"><Sw v-model="settings.state.editorWordWrap" /></div>
            </div>
            <div class="srow">
              <div class="srow-txt"><b>缩略图</b><p>右侧的代码缩略图，窄窗口下建议关闭。</p></div>
              <div class="srow-ctl"><Sw v-model="settings.state.editorMinimap" /></div>
            </div>
            <div class="srow">
              <div class="srow-txt"><b>缩进宽度</b></div>
              <div class="srow-ctl">
                <input v-model.number="settings.state.editorTabSize" class="input w-64" type="number" min="1" max="8" />
              </div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>打开时格式化 JSON</b>
                <p>识别出 JSON 时自动缩进。不会写回服务端，除非你点保存。</p>
              </div>
              <div class="srow-ctl"><Sw v-model="settings.state.formatJsonOnOpen" /></div>
            </div>
          </div>
        </template>

        <!-- ── 连接与安全 ── -->
        <template v-else-if="section === 'security'">
          <h3>连接与安全</h3>
          <p class="lead">凭据存储与已保存的连接。</p>
          <div class="banner banner-info" style="margin-top: 12px">
            <Icon name="lock" :size="16" />
            <span>
              密码、SSH 口令、哨兵节点密码都存在操作系统的密钥链里，
              连接配置文件中只留一个「是否已保存」的标志。
            </span>
          </div>
          <div class="banner banner-warn" style="margin-top: 10px">
            <Icon name="shield" :size="16" />
            <span>
              RSA 私钥暂不可用：依赖的 Rust 加密库对 RUSTSEC-2023-0071 尚无修复版本。
              SSH 请改用 Ed25519 或 ECDSA。
            </span>
          </div>

          <div class="set-group">
            <div class="cap">已保存的连接（{{ connections.items.length }}）</div>
            <div v-for="item in connections.items" :key="item.key" class="srow">
              <div class="srow-txt">
                <b>{{ item.name }}</b>
                <p class="mono">{{ item.host }}:{{ item.port }}</p>
              </div>
              <div class="srow-ctl">
                <span v-if="item.readonly" class="tag tag-danger">只读</span>
                <span v-if="item.hasAuth" class="tag tag-ok">密码</span>
                <span v-if="item.tlsEnabled" class="tag">TLS</span>
                <span v-if="item.sshEnabled" class="tag">SSH</span>
                <Btn size="sm" icon="pencil" icon-only title="编辑" @click="ui.openConnectionDialog(item.key)" />
              </div>
            </div>
          </div>
        </template>

        <!-- ── 数据查看器 ── -->
        <template v-else-if="section === 'viewer'">
          <h3>数据查看器</h3>
          <p class="lead">值的编解码能力。</p>
          <table class="dt" style="margin-top: 12px">
            <thead>
              <tr>
                <th style="width: 180px">编解码器</th>
                <th style="width: 160px">声明能力</th>
                <th style="width: 120px">当前状态</th>
                <!-- 「能不能跑」与「能不能自动识别」是两个维度，摆成两列才看得出来 -->
                <th>自动识别</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="codec in CODECS" :key="codec.name">
                <td>{{ codec.label }}</td>
                <td class="muted">{{ CONCLUSION_LABEL[codec.conclusion] }}</td>
                <td>
                  <span v-if="isImplemented(codec.name)" class="tag tag-ok">可用</span>
                  <span v-else class="tag tag-warn">未实现</span>
                </td>
                <td class="muted">{{ codec.detectable ? '在候选集里' : '需手动指定' }}</td>
              </tr>
            </tbody>
          </table>
          <!-- 声明的能力和已实现的能力是两回事，别让用户点到一个必然报错的选项 -->
          <div class="banner banner-info" style="margin-top: 12px">
            <Icon name="info" :size="16" />
            <span>
              四种压缩算法可解可编。序列化格式里 MessagePack 与 Protobuf 没有可靠的
              魔数 —— 任意字节都可能是合法的 MessagePack，所以它们不进自动识别的候选，
              只能手动指定。Python pickle 与 Java 序列化只做纯字节结构解析：不加载类、
              不实例化对象，也不调用反序列化钩子；出于安全考虑，它们永远不会编码写回。
              未实现的一律置灰并注明，而不是等到点下去才报错。
            </span>
          </div>
        </template>

        <!-- ── 格式化器 ── -->
        <template v-else-if="section === 'formatter'">
          <h3>格式化器</h3>
          <p class="lead">用外部程序处理值。值以十六进制写进临时文件后传给它。</p>
          <div class="set-group">
            <div class="srow">
              <div class="srow-txt"><b>命令</b><p>可执行文件的路径。</p></div>
              <div class="srow-ctl">
                <input v-model="settings.state.formatterCommand" class="input mono w-160" placeholder="C:\tools\fmt.exe" />
              </div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>参数模板</b>
                <p>支持 {file} {key} {field} {score} {member} 占位符。</p>
              </div>
              <div class="srow-ctl">
                <input v-model="settings.state.formatterParams" class="input mono w-160" placeholder="--input {file}" />
              </div>
            </div>
            <div class="srow">
              <div class="srow-txt"><b>超时</b></div>
              <div class="srow-ctl">
                <input v-model.number="settings.state.formatterTimeoutMs" class="input w-120" type="number" min="100" step="100" />
                <span class="muted" style="font-size: var(--fs-11)">毫秒</span>
              </div>
            </div>
            <div class="srow">
              <div class="srow-txt"><b>输出上限</b><p>超出部分会被截断。</p></div>
              <div class="srow-ctl">
                <input v-model.number="settings.state.formatterMaxOutputBytes" class="input w-120" type="number" min="1024" step="1024" />
                <span class="muted" style="font-size: var(--fs-11)">字节</span>
              </div>
            </div>
          </div>

          <div class="banner banner-warn" style="margin-top: 12px">
            <Icon name="shield" :size="16" />
            <span>
              Windows 上 <code>.bat</code> 与 <code>.cmd</code> 会被后端直接拒绝 ——
              它们的参数转义规则容易被构造成命令注入。这条限制不可绕过。
            </span>
          </div>

          <div class="set-group">
            <div class="cap">试运行</div>
            <div class="field" style="grid-template-columns: 80px minmax(0, 1fr)">
              <label>示例值</label>
              <div class="field-ctl">
                <input v-model="formatterSample" class="input mono grow" />
                <Btn
                  size="sm"
                  :disabled="!settings.state.formatterCommand || formatterBusy"
                  @click="testFormatter"
                >
                  运行
                </Btn>
              </div>
            </div>
            <div v-if="formatterResult" class="banner" :class="formatterResult.ok ? 'banner-ok' : 'banner-danger'" style="margin-top: 8px">
              <Icon :name="formatterResult.ok ? 'circle-check' : 'alert-circle'" :size="16" />
              <div style="min-width: 0">
                <div v-if="formatterResult.preview" class="mono selectable" style="font-size: var(--fs-10); opacity: .8">
                  {{ formatterResult.preview }}
                </div>
                <pre class="mono selectable" style="margin-top: 4px; white-space: pre-wrap; word-break: break-word">{{ formatterResult.text }}</pre>
              </div>
            </div>
          </div>
        </template>

        <!-- ── 快捷键 ── -->
        <template v-else-if="section === 'shortcuts'">
          <h3>快捷键</h3>
          <p class="lead">v0.1 的快捷键是固定的，尚不支持自定义。</p>
          <table class="dt" style="margin-top: 12px">
            <thead>
              <tr><th style="width: 200px">操作</th><th>按键</th></tr>
            </thead>
            <tbody>
              <tr><td>新建 Key 浏览器</td><td><span class="kbd">Ctrl</span> <span class="kbd">B</span></td></tr>
              <tr><td>新建命令行</td><td><span class="kbd">Ctrl</span> <span class="kbd">`</span></td></tr>
              <tr><td>关闭当前标签页</td><td><span class="kbd">Ctrl</span> <span class="kbd">W</span>（输入框内不生效）</td></tr>
              <tr><td>刷新当前视图</td><td><span class="kbd">F5</span></td></tr>
              <tr><td>树内导航</td><td><span class="kbd">↑</span> <span class="kbd">↓</span> <span class="kbd">←</span> <span class="kbd">→</span></td></tr>
              <tr><td>命令历史</td><td><span class="kbd">↑</span> <span class="kbd">↓</span>（CLI 内）</td></tr>
              <tr><td>删除前一个词</td><td><span class="kbd">Ctrl</span> <span class="kbd">W</span>（CLI 内）</td></tr>
              <tr><td>清屏</td><td><span class="kbd">Ctrl</span> <span class="kbd">L</span>（CLI 内）</td></tr>
            </tbody>
          </table>
        </template>

        <!-- ── 关于 ── -->
        <template v-else>
          <h3>关于</h3>
          <p class="lead">Tsubasa Redis Manager · {{ version }}</p>

          <div class="set-group">
            <div class="cap">项目</div>
            <div class="srow">
              <div class="srow-txt">
                <b>项目主页</b>
                <p>github.com/AoShizuku/TsubasaRedisManager</p>
              </div>
              <div class="srow-ctl">
                <Btn
                  size="sm"
                  variant="ghost"
                  icon="external-link"
                  @click="openExternal('https://github.com/AoShizuku/TsubasaRedisManager')"
                >
                  打开
                </Btn>
              </div>
            </div>
          </div>

          <div class="set-group">
            <div class="cap">许可</div>
            <div class="srow">
              <div class="srow-txt">
                <b>MIT License</b>
                <p>Copyright © 2026 AoShizuku。本程序按「原样」提供，不附带任何担保。</p>
              </div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>第三方组件</b>
                <p>下列组件各自的许可与版权声明随本程序一同分发。</p>
                <dl class="credits">
                  <template v-for="item in CREDITS" :key="item.name">
                    <dt>{{ item.name }}</dt>
                    <dd>{{ item.license }}</dd>
                  </template>
                </dl>
              </div>
              <div class="srow-ctl">
                <Btn size="sm" icon="file-text" @click="showLicenses">查看全文</Btn>
              </div>
            </div>
            <!-- CC BY 4.0 要求署名必须传达给接收者，所以它得出现在这里而不是
                 只躺在仓库的声明文件里。monaco 自己的 LICENSE 没提这件事。 -->
            <div class="srow">
              <div class="srow-txt">
                <b>Codicon 图标字体</b>
                <p>
                  © Microsoft Corporation，依据 CC BY 4.0 授权，随 Monaco Editor 一同分发。
                </p>
              </div>
              <div class="srow-ctl">
                <Btn
                  size="sm"
                  variant="ghost"
                  icon="external-link"
                  @click="openExternal('https://creativecommons.org/licenses/by/4.0/')"
                >
                  许可条款
                </Btn>
              </div>
            </div>
          </div>

          <div class="set-group">
            <div class="cap">更新与重置</div>
            <div class="srow">
              <div class="srow-txt">
                <b>更新检查地址</b>
                <p>留空则不检查更新。</p>
              </div>
              <div class="srow-ctl">
                <input v-model="settings.state.updateEndpoint" class="input mono w-160" placeholder="https://…" />
                <Btn size="sm" :disabled="updateBusy" @click="doCheckUpdate">检查</Btn>
              </div>
            </div>
            <div v-if="update" class="srow">
              <div class="srow-txt">
                <b>
                  <template v-if="update.status === 'available'">有新版本 {{ update.latestVersion }}</template>
                  <template v-else-if="update.status === 'notAvailable'">已是最新版本</template>
                  <template v-else-if="update.status === 'notConfigured'">未配置更新地址</template>
                  <template v-else>检查失败</template>
                </b>
                <p>当前版本 {{ update.currentVersion }}{{ update.message ? ` · ${update.message}` : '' }}</p>
              </div>
              <div class="srow-ctl">
                <Btn
                  v-if="update.releaseUrl"
                  size="sm"
                  icon="external-link"
                  @click="openExternal(update.releaseUrl)"
                >
                  查看
                </Btn>
              </div>
            </div>
            <div class="srow">
              <div class="srow-txt">
                <b>恢复默认设置</b>
                <p>只影响界面偏好，不会动已保存的连接。</p>
              </div>
              <div class="srow-ctl"><Btn size="sm" variant="danger" @click="resetAll">恢复默认</Btn></div>
            </div>
          </div>
        </template>
      </div>
    </div>
  </Dlg>
</template>
