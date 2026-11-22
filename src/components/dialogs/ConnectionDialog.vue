<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import Btn from '@/components/ui/Btn.vue'
import Cbx from '@/components/ui/Cbx.vue'
import Dlg from '@/components/ui/Dlg.vue'
import Icon from '@/components/ui/Icon.vue'
import Sw from '@/components/ui/Sw.vue'
import {
  CONNECTION_COLORS,
  draftFromForm,
  emptyForm,
  formFromDraft,
  validate,
  type ConnectionForm,
} from './connectionDraft'
import { getConnection } from '@/api/connections'
import { openConnection as openRawConnection, closeConnection } from '@/api/redis'
import { useConnectionsStore } from '@/stores/connections'
import { useSessionsStore } from '@/stores/sessions'
import { useTabsStore } from '@/stores/tabs'
import { useUiStore } from '@/stores/ui'

const connections = useConnectionsStore()
const sessions = useSessionsStore()
const tabs = useTabsStore()
const ui = useUiStore()

type TabKey = 'general' | 'tls' | 'ssh' | 'sentinel' | 'advanced'

const TABS: Array<{ key: TabKey; label: string }> = [
  { key: 'general', label: '常规' },
  { key: 'tls', label: 'TLS' },
  { key: 'ssh', label: 'SSH' },
  { key: 'sentinel', label: '哨兵' },
  { key: 'advanced', label: '高级' },
]

const tab = ref<TabKey>('general')
const form = ref<ConnectionForm>(emptyForm())
const saving = ref(false)
const testing = ref(false)
const testResult = ref<{ ok: boolean; text: string } | null>(null)

const open = computed({
  get: () => ui.connectionDialog.open,
  set: (value: boolean) => {
    if (!value) ui.closeConnectionDialog()
  },
})

const isEdit = computed(() => !!form.value.key)
const issues = computed(() => validate(form.value))
const issuesFor = (key: TabKey) => issues.value.filter((issue) => issue.tab === key)

watch(
  () => ui.connectionDialog,
  async (state) => {
    if (!state.open) return
    tab.value = 'general'
    testResult.value = null
    if (!state.key) {
      form.value = emptyForm()
      return
    }
    try {
      form.value = formFromDraft(await getConnection(state.key))
    } catch (error) {
      ui.notifyError(error, '读取连接配置失败')
      ui.closeConnectionDialog()
    }
  },
  { deep: true },
)

/**
 * 测试连接：真的开一次再立刻关掉。
 * 编辑已存连接时如果密码留空（表示"沿用密钥链里的"），这里拿不到它，
 * 所以测的是"当前表单里填的东西"，会如实说明。
 */
async function test() {
  if (issues.value.length) {
    tab.value = issues.value[0].tab
    return
  }
  testing.value = true
  testResult.value = null
  try {
    const draft = draftFromForm(form.value)
    const opened = await openRawConnection({
      host: draft.host,
      port: draft.port,
      auth: draft.auth,
      username: draft.username,
      db: draft.db,
      name: draft.name,
      separator: draft.separator,
      connectionReadOnly: draft.connectionReadOnly,
      cluster: draft.cluster,
      sslOptions: draft.sslOptions,
      sshOptions: draft.sshOptions
        ? {
            host: draft.sshOptions.host,
            port: draft.sshOptions.port,
            username: draft.sshOptions.username,
            password: draft.sshOptions.password,
            privatekey: draft.sshOptions.privatekey,
            passphrase: draft.sshOptions.passphrase,
            timeout: draft.sshOptions.timeout,
          }
        : null,
      sentinelOptions: draft.sentinelOptions
        ? {
            masterName: draft.sentinelOptions.masterName,
            nodePassword: draft.sentinelOptions.nodePassword,
          }
        : null,
    })
    await closeConnection(opened.connectionId)
    testResult.value = { ok: true, text: `连接成功：${opened.connectionName}` }
  } catch (error) {
    const fault = ui.notifyError(error, '连接测试失败')
    testResult.value = { ok: false, text: fault.text }
  } finally {
    testing.value = false
  }
}

async function save() {
  if (issues.value.length) {
    tab.value = issues.value[0].tab
    return
  }
  saving.value = true
  try {
    const saved = await connections.save(draftFromForm(form.value))
    sessions.invalidateDraft(saved.key)

    /*
     * 已经开着的会话必须一起作废。会话在 redis_open_connection 那一刻就把
     * 只读、db、凭据全部定死了，改配置不会追溯到它身上 —— 不断开的话，
     * 把连接改成只读后，那个旧会话的 writable_runtime 仍然放行写命令。
     * 侧栏折叠连接并不断开会话，所以"看起来没连着"也可能有活的会话。
     * 删除连接那条路径同理，它也是先 disconnect 再删。
     */
    const affected = sessions.sessionIdsFor(saved.key)
    for (const id of affected) tabs.closeBySession(id)
    await sessions.closeConnection(saved.key)

    if (affected.length) {
      ui.notify('warn', '配置已更新，该连接已断开，请重新连接')
    } else {
      ui.notify('ok', isEdit.value ? '连接已更新' : '连接已保存')
    }
    ui.closeConnectionDialog()
  } catch (error) {
    ui.notifyError(error, '保存失败')
  } finally {
    saving.value = false
  }
}
</script>

<template>
  <Dlg
    v-model:open="open"
    :title="isEdit ? '编辑连接' : '新建连接'"
    :description="isEdit ? form.name || form.host : '密码与私钥口令保存在系统密钥链，不写进配置文件'"
    :width="620"
    :busy="saving"
  >
    <template #tabs>
      <div class="dlg-tabs">
        <button
          v-for="item in TABS"
          :key="item.key"
          class="dlg-tab"
          :class="{ 'is-active': tab === item.key }"
          @click="tab = item.key"
        >
          {{ item.label }}
          <span v-if="issuesFor(item.key).length" class="tag tag-danger">!</span>
          <span v-else-if="item.key === 'tls' && form.tlsEnabled" class="tag tag-ok">开</span>
          <span v-else-if="item.key === 'ssh' && form.sshEnabled" class="tag tag-ok">开</span>
          <span v-else-if="item.key === 'sentinel' && form.sentinelEnabled" class="tag tag-ok">开</span>
        </button>
      </div>
    </template>

    <!-- ── 常规 ── -->
    <div v-if="tab === 'general'" class="form">
      <div class="field">
        <label>名称</label>
        <div class="field-ctl">
          <input v-model="form.name" class="input" placeholder="留空则用 主机:端口/db" />
        </div>
      </div>
      <div class="field">
        <label>主机</label>
        <div class="field-ctl">
          <input v-model="form.host" class="input mono grow" placeholder="127.0.0.1" />
          <input v-model.number="form.port" class="input mono w-88" type="number" min="1" max="65535" />
        </div>
      </div>
      <div class="field">
        <label>用户名</label>
        <div class="field-ctl">
          <input v-model="form.username" class="input" placeholder="Redis 6 之前留空" />
        </div>
      </div>
      <div class="field">
        <label>密码</label>
        <div class="field-ctl">
          <input
            v-model="form.auth"
            class="input grow"
            type="password"
            :placeholder="form.hasAuth ? '已保存，留空则保持不变' : ''"
            :disabled="form.clearAuth"
          />
          <span v-if="form.hasAuth" class="tag tag-ok" title="保存在系统密钥链">
            <Icon name="lock" :size="12" />
            密钥链
          </span>
        </div>
      </div>
      <div v-if="form.hasAuth" class="field">
        <label />
        <div class="field-ctl">
          <Cbx v-model="form.clearAuth" />
          <span style="font-size: var(--fs-12)">从密钥链中删除已保存的密码</span>
        </div>
      </div>
      <div class="field">
        <label>数据库</label>
        <div class="field-ctl">
          <input v-model.number="form.db" class="input w-88" type="number" min="0" :disabled="form.cluster" />
          <span v-if="form.cluster" class="muted" style="font-size: var(--fs-11)">集群模式只有 db0</span>
        </div>
      </div>
      <div class="field">
        <label>标记颜色</label>
        <div class="field-ctl swatches">
          <button
            v-for="color in CONNECTION_COLORS"
            :key="color || 'none'"
            class="swatch"
            :class="{ 'is-on': form.color === color }"
            :style="{ background: color || 'var(--bg-inset)' }"
            :title="color || '不标记'"
            @click="form.color = color"
          />
        </div>
      </div>
      <div class="field">
        <label>只读模式</label>
        <div class="field-ctl">
          <Sw v-model="form.readonly" />
          <span class="muted" style="font-size: var(--fs-11)">
            拦截所有写命令。生产库建议开启。
          </span>
        </div>
      </div>
    </div>

    <!-- ── TLS ── -->
    <div v-else-if="tab === 'tls'" class="form">
      <div class="field">
        <label>启用 TLS</label>
        <div class="field-ctl"><Sw v-model="form.tlsEnabled" /></div>
      </div>
      <template v-if="form.tlsEnabled">
        <div class="field">
          <label>CA 证书</label>
          <div class="field-ctl">
            <input v-model="form.tls.ca" class="input mono" placeholder="ca.crt 的路径" />
          </div>
        </div>
        <div class="field">
          <label>客户端证书</label>
          <div class="field-ctl">
            <input v-model="form.tls.cert" class="input mono" placeholder="client.crt 的路径" />
          </div>
        </div>
        <div class="field">
          <label>客户端私钥</label>
          <div class="field-ctl">
            <input v-model="form.tls.key" class="input mono" placeholder="client.key 的路径" />
          </div>
        </div>
        <div class="field">
          <label>SNI 主机名</label>
          <div class="field-ctl">
            <input v-model="form.tls.servername" class="input mono" placeholder="证书里的主机名与连接地址不同时填写" />
          </div>
        </div>
        <div class="field">
          <label>跳过证书校验</label>
          <div class="field-ctl">
            <Sw v-model="form.tls.insecure as boolean" />
          </div>
        </div>
        <div v-if="form.tls.insecure" class="banner banner-danger" style="margin-top: 10px">
          <Icon name="alert-triangle" :size="16" />
          <span>关闭证书校验后，连接可被中间人劫持。只在自签测试环境里用。</span>
        </div>
      </template>
    </div>

    <!-- ── SSH ── -->
    <div v-else-if="tab === 'ssh'" class="form">
      <div class="field">
        <label>启用 SSH 隧道</label>
        <div class="field-ctl"><Sw v-model="form.sshEnabled" /></div>
      </div>
      <template v-if="form.sshEnabled">
        <div class="field">
          <label>跳板机</label>
          <div class="field-ctl">
            <input v-model="form.ssh.host" class="input mono grow" placeholder="bastion.example.com" />
            <input v-model.number="form.ssh.port" class="input mono w-88" type="number" min="1" max="65535" />
          </div>
        </div>
        <div class="field">
          <label>用户名</label>
          <div class="field-ctl"><input v-model="form.ssh.username" class="input" /></div>
        </div>
        <div class="field">
          <label>密码</label>
          <div class="field-ctl">
            <input
              v-model="form.ssh.password"
              class="input grow"
              type="password"
              :placeholder="form.hasSshPassword ? '已保存，留空则保持不变' : ''"
            />
            <span v-if="form.hasSshPassword" class="tag tag-ok">
              <Icon name="lock" :size="12" />
              密钥链
            </span>
          </div>
        </div>
        <div class="field">
          <label>私钥</label>
          <div class="field-ctl">
            <input v-model="form.ssh.privatekey" class="input mono" placeholder="~/.ssh/id_ed25519" />
          </div>
        </div>
        <div class="field">
          <label>私钥口令</label>
          <div class="field-ctl">
            <input
              v-model="form.ssh.passphrase"
              class="input grow"
              type="password"
              :placeholder="form.hasSshPassphrase ? '已保存，留空则保持不变' : ''"
            />
            <span v-if="form.hasSshPassphrase" class="tag tag-ok">
              <Icon name="lock" :size="12" />
              密钥链
            </span>
          </div>
        </div>
        <div class="field">
          <label>超时</label>
          <div class="field-ctl">
            <input v-model.number="form.ssh.timeout" class="input w-88" type="number" min="1" />
            <span class="muted" style="font-size: var(--fs-11)">秒</span>
          </div>
        </div>

        <!-- 这条限制来自依赖而不是产品选择，必须写清楚出处 -->
        <div class="banner banner-warn" style="margin-top: 12px">
          <Icon name="shield" :size="16" />
          <span>
            RSA 私钥暂不可用：所依赖的 Rust 加密库对 RUSTSEC-2023-0071 尚无修复版本。
            请改用 Ed25519 或 ECDSA 私钥。
          </span>
        </div>
      </template>
    </div>

    <!-- ── 哨兵 ── -->
    <div v-else-if="tab === 'sentinel'" class="form">
      <div class="field">
        <label>启用哨兵</label>
        <div class="field-ctl">
          <Sw v-model="form.sentinelEnabled" :disabled="form.cluster" />
          <span v-if="form.cluster" class="muted" style="font-size: var(--fs-11)">
            已启用集群模式
          </span>
        </div>
      </div>
      <template v-if="form.sentinelEnabled">
        <div class="field">
          <label>Master name</label>
          <div class="field-ctl">
            <input v-model="form.sentinel.masterName" class="input mono" placeholder="mymaster" />
          </div>
        </div>
        <div class="field">
          <label>节点密码</label>
          <div class="field-ctl">
            <input
              v-model="form.sentinel.nodePassword"
              class="input grow"
              type="password"
              :placeholder="form.hasSentinelNodePassword ? '已保存，留空则保持不变' : '数据节点与哨兵密码不同时填写'"
            />
            <span v-if="form.hasSentinelNodePassword" class="tag tag-ok">
              <Icon name="lock" :size="12" />
              密钥链
            </span>
          </div>
        </div>
        <div class="field-hint">
          上面「常规」里填的主机与密码用于连接哨兵本身，这里填的是它管理的数据节点。
        </div>
      </template>
    </div>

    <!-- ── 高级 ── -->
    <div v-else class="form">
      <div class="field">
        <label>集群模式</label>
        <div class="field-ctl">
          <Sw v-model="form.cluster" :disabled="form.sentinelEnabled" />
          <span v-if="form.sentinelEnabled" class="muted" style="font-size: var(--fs-11)">
            已启用哨兵
          </span>
        </div>
      </div>
      <div class="field">
        <label>命名空间分隔符</label>
        <div class="field-ctl">
          <input v-model="form.separator" class="input mono w-88" maxlength="4" />
          <span class="muted" style="font-size: var(--fs-11)">
            决定 Key 树怎么折叠层级；留空则不折叠
          </span>
        </div>
      </div>

      <div v-if="form.cluster && form.sentinelEnabled" class="banner banner-danger" style="margin-top: 12px">
        <Icon name="alert-triangle" :size="16" />
        <span>集群与哨兵不能同时启用 —— 后端在建立连接计划时会直接拒绝这种组合。</span>
      </div>
    </div>

    <template #footer>
      <Btn :disabled="testing || saving" @click="test">
        <Icon v-if="testing" name="loader" :size="16" />
        测试连接
      </Btn>
      <span
        v-if="testResult"
        :style="{ fontSize: 'var(--fs-11)', color: testResult.ok ? 'var(--ok)' : 'var(--brand)' }"
        class="clip"
      >
        {{ testResult.text }}
      </span>
      <span class="spacer" />
      <span v-if="issues.length" class="muted" style="font-size: var(--fs-11); color: var(--brand)">
        {{ issues[0].message }}
      </span>
      <Btn @click="open = false">取消</Btn>
      <Btn variant="primary" :disabled="saving || issues.length > 0" @click="save">保存</Btn>
    </template>
  </Dlg>
</template>
