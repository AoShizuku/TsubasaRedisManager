/**
 * Monaco 的加载入口。
 *
 * 单独一个文件而不是并进 `lib/monaco.ts`：那个文件静态 import 了 monaco 本身，
 * 把这个函数放进去，光是**调用**它就已经要求把整包加载完 —— 正好抵消掉动态
 * import 的意义。这里只装一个 promise，不碰 monaco。
 *
 * ESM 对同一个说明符本来就会去重，所以缓存 promise 不是为了省下重复请求，
 * 而是为了让**预热**和真正打开编辑器落在同一条路径上：预热正在跑的时候用户
 * 点开一个 String，await 的是同一个 promise 而不是另起一条。
 */
let pending: Promise<typeof import('./monaco')> | null = null

export function loadMonaco(): Promise<typeof import('./monaco')> {
  pending ??= import('./monaco').catch((error) => {
    // 失败不要粘在缓存里。加载的是随程序分发的本地 chunk，失败多半是一次性的
    // （例如更新时旧 chunk 正好被替换掉），下次打开还该有机会成功
    pending = null
    throw error
  })
  return pending
}
