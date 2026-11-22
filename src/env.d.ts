/**
 * 构建期注入的常量。
 *
 * `__APP_VERSION__` 由 vite.config.ts 从 `src-tauri/tauri.conf.json` 读出来，
 * 不要改成从 package.json 取 —— 打包出来的应用认的是 tauri.conf.json 那个版本。
 */
declare const __APP_VERSION__: string
