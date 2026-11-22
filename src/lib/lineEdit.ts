/**
 * CLI 输入框的行编辑。
 *
 * 只补 readline 里手指记得最牢的那几个键，不是要把行编辑器实现全 ——
 * CLI 是请求/响应而不是 PTY，见 components/cli/CliTab.vue 顶部。
 */

/** 编辑后的新文本与光标落点 */
export interface LineEdit {
  text: string
  caret: number
}

/**
 * readline 的 unix-word-rubout（Ctrl+W）：先吃掉光标前的空白，再吃掉一个词。
 *
 * `from` / `to` 是选区两端。**有选区时只吃掉选区** —— 删除动作先消费选区是
 * 所有编辑器的语义，再顺带删掉前面一个词会让人莫名其妙少一截命令。
 */
export function killWordBefore(text: string, from: number, to: number = from): LineEdit {
  const head = from === to ? text.slice(0, from).replace(/\S*\s*$/, '') : text.slice(0, from)
  return { text: head + text.slice(to), caret: head.length }
}
