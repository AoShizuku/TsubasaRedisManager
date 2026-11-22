import { readFileSync } from 'node:fs'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

/* 版本号取 tauri.conf.json 而不是 package.json：前者才是打包出来那个应用的
   版本，后者只是 npm 侧的记账。设置里的「关于」要显示它。 */
const appVersion = JSON.parse(
  readFileSync(new URL('./src-tauri/tauri.conf.json', import.meta.url), 'utf8'),
).version as string

export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  define: {
    __APP_VERSION__: JSON.stringify(appVersion),
  },
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  server: {
    host: '127.0.0.1',
    port: 1420,
    strictPort: true,
  },
  /*
   * **不要给 Monaco 加 `manualChunks`。** 默认的动态 import 分块已经把它拆成
   * 独立的异步块了（`editor.api` + `jsonMode`），而显式声明一个 monaco 手动块
   * 会做反：Vite 的 preload 辅助函数被归进那个块，入口于是**静态** import 它，
   * 结果 3.9 MB 的编辑器加 161 KB 的 CSS 一起进了 index.html 的 modulepreload，
   * 首屏必载 —— 正好是这条配置本来想避免的事，而且没有任何报错提示。
   *
   * 首个 String 的等待由 `lib/monacoLoader` 的空闲预热来抵，不靠打包分块。
   */
})
