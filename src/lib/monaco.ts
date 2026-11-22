/**
 * Monaco 的按需装配。
 *
 * 不 import 'monaco-editor' 整包：它的 esm 入口会把 80 多种语言的定义
 * 全部拉进来，而这里只用得上 JSON 与纯文本。所以只取 editor.api 加上
 * 明确需要的 contrib 模块 —— 每条路径都对着 node_modules 核过。
 *
 * 注意包的 exports 映射是 `"./*" → "./esm/vs/*.js"`，
 * 所以说明符要写成 `monaco-editor/editor/...` 而不是 `monaco-editor/esm/vs/editor/...`。
 */
import * as monaco from 'monaco-editor/editor/editor.api'

/* 编辑器功能：查找、折叠、括号匹配、右键菜单、格式化、多光标… */
import 'monaco-editor/editor/contrib/find/browser/findController'
import 'monaco-editor/editor/contrib/folding/browser/folding'
import 'monaco-editor/editor/contrib/bracketMatching/browser/bracketMatching'
import 'monaco-editor/editor/contrib/contextmenu/browser/contextmenu'
import 'monaco-editor/editor/contrib/format/browser/formatActions'
import 'monaco-editor/editor/contrib/wordHighlighter/browser/wordHighlighter'
import 'monaco-editor/editor/contrib/clipboard/browser/clipboard'
import 'monaco-editor/editor/contrib/cursorUndo/browser/cursorUndo'
import 'monaco-editor/editor/contrib/linesOperations/browser/linesOperations'
import 'monaco-editor/editor/contrib/multicursor/browser/multicursor'
import 'monaco-editor/editor/contrib/comment/browser/comment'
import 'monaco-editor/editor/contrib/links/browser/links'
import 'monaco-editor/editor/contrib/hover/browser/hoverContribution'

/* JSON 语言服务：校验、格式化、括号折叠 */
import 'monaco-editor/language/json/monaco.contribution'

import EditorWorker from 'monaco-editor/editor/editor.worker?worker'
import JsonWorker from 'monaco-editor/language/json/json.worker?worker'

/**
 * worker 用 Vite 的 ?worker 产物（同源模块），不是 blob。
 * CSP 里 worker-src 是 'self' blob:，两种都放行，但同源产物更省事。
 */
self.MonacoEnvironment = {
  getWorker(_workerId: string, label: string) {
    return label === 'json' ? new JsonWorker() : new EditorWorker()
  },
}

let themeReady = false

/** 与 tokens.css 对齐的浅色主题。深色主题不在 v0.1 范围内。 */
function ensureTheme() {
  if (themeReady) return
  monaco.editor.defineTheme('tsubasa-light', {
    base: 'vs',
    inherit: true,
    rules: [
      { token: 'string.key.json', foreground: '0451a5' },
      { token: 'string.value.json', foreground: 'a31515' },
      { token: 'string', foreground: 'a31515' },
      { token: 'number', foreground: '098658' },
      { token: 'keyword', foreground: '0000ff' },
      { token: 'comment', foreground: '3d8a3d' },
    ],
    colors: {
      'editor.background': '#ffffff',
      'editor.foreground': '#1f2328',
      'editorLineNumber.foreground': '#b0b0ba',
      'editorLineNumber.activeForeground': '#55555f',
      'editor.selectionBackground': '#dbe7fb',
      'editor.lineHighlightBackground': '#fbfbfc',
      'editorIndentGuide.background1': '#e6e6ea',
      'editorGutter.background': '#ffffff',
    },
  })
  themeReady = true
}

export interface EditorHandle {
  getValue(): string
  setValue(value: string): void
  setLanguage(language: string): void
  format(): Promise<void>
  updateOptions(options: monaco.editor.IEditorOptions): void
  layout(): void
  dispose(): void
  onChange(handler: () => void): void
}

export function createEditor(
  host: HTMLElement,
  options: { value: string; language: string; readOnly: boolean; wordWrap: boolean; tabSize: number; minimap: boolean },
): EditorHandle {
  ensureTheme()

  const editor = monaco.editor.create(host, {
    value: options.value,
    language: options.language,
    theme: 'tsubasa-light',
    readOnly: options.readOnly,
    wordWrap: options.wordWrap ? 'on' : 'off',
    tabSize: options.tabSize,
    minimap: { enabled: options.minimap },
    automaticLayout: true,
    fontSize: 12.5,
    lineHeight: 20,
    // 与全局细滚动条统一；Monaco 自己的滚动条宽度得单独设
    scrollbar: { verticalScrollbarSize: 10, horizontalScrollbarSize: 10, useShadows: false },
    fontFamily: getComputedStyle(document.documentElement).getPropertyValue('--ff-mono-user'),
    renderLineHighlight: 'line',
    scrollBeyondLastLine: false,
    smoothScrolling: false,
    padding: { top: 10, bottom: 10 },
    fixedOverflowWidgets: true,
  })

  return {
    getValue: () => editor.getValue(),
    setValue: (value) => editor.setValue(value),
    setLanguage: (language) => {
      const model = editor.getModel()
      if (model) monaco.editor.setModelLanguage(model, language)
    },
    format: async () => {
      await editor.getAction('editor.action.formatDocument')?.run()
    },
    updateOptions: (next) => editor.updateOptions(next),
    layout: () => editor.layout(),
    dispose: () => {
      editor.getModel()?.dispose()
      editor.dispose()
    },
    onChange: (handler) => {
      editor.onDidChangeModelContent(() => handler())
    },
  }
}
