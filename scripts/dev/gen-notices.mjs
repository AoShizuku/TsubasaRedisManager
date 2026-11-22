// 生成 THIRD_PARTY_NOTICES.md。依赖变更后重跑：
//
//   node scripts/dev/gen-notices.mjs
//
// 为什么要有这个脚本：MIT / ISC / BSD / Apache-2.0 都要求分发时**复制**版权
// 声明与许可证文本，只在表格里写一个 "MIT" 是不够的。手工维护 600 多个 crate
// 的声明不可能保持正确，所以从 cargo metadata 与 package-lock.json 直接生成。
//
// 只收集真正随二进制分发的依赖：Cargo 侧沿 normal 边遍历（跳过 dev 与 build
// 依赖），npm 侧读 package-lock 的 packages 并剔除 dev。构建期工具不进产物，
// 也就不产生分发义务。
//
// 许可证全文按**内容**去重：Apache-2.0 全文有 11 KB，几百个 crate 各贴一份会
// 让文件到几 MB。同一份文本只出现一次，并列出共享它的所有包。每个包各自的
// 版权行因此仍然逐字保留 —— 那正是 MIT 要求的部分。
//
// 产出两个文件而不是一个：GitHub 超过 512 KB 就不渲染 Markdown 了，清单和全文
// 挤在一起谁都看不到。清单留在 .md 供人读，全文进 .txt 承担分发义务。

import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const LICENSE_FILE = /^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE)([-_.].*)?$/i

/**
 * 手写的注记。生成器覆盖不到这些 —— 它只会读包目录里的许可证文件，
 * 而下面两条一条藏在包内的资源里、一条是关于**没有**引入什么的决策。
 * 改依赖时顺带复核这一节，它不会自己更新。
 */
const MANUAL_NOTES = `## Additional notices

\`monaco-editor\` bundles the Codicon icon font, which Microsoft distributes under
CC BY 4.0 rather than under monaco's own MIT license — the package's LICENSE file
does not mention it, so a generator reading license files alone would miss it.
CC BY 4.0 requires attribution to be given to recipients, which for a GUI
application means the About screen, not just this file.

No third-party source has been copied into this repository. The interface is
styled with stylesheets authored here; \`reka-ui\` supplies unstyled behavioral
primitives and is consumed as an ordinary npm dependency.

shadcn-vue was deliberately **not** vendored. Its distribution model is to copy
component source into the consuming repository, which would place third-party
source under version control and require reproducing its copyright and
permission notice here. Depending on \`reka-ui\` — the primitive layer shadcn-vue
itself builds on — keeps that source in \`node_modules\`, which is not committed.
If that decision is ever reversed, every copied file's notice must be recorded.
`

/** 包目录里随包分发的许可证文件。名字五花八门，按模式收而不是猜。 */
function licenseTexts(dir) {
  let entries
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true })
  } catch {
    return []
  }
  return entries
    .filter((e) => e.isFile() && LICENSE_FILE.test(e.name))
    .sort((a, b) => a.name.localeCompare(b.name))
    .map((e) => {
      const text = fs.readFileSync(path.join(dir, e.name), 'utf8').replace(/^﻿/, '')
      return { file: e.name, text: text.replace(/\r\n/g, '\n').trim() }
    })
    .filter((entry) => entry.text.length > 0)
}

/* ── Cargo ───────────────────────────────────────────────
   resolve.nodes 里每条边带 dep_kinds；kind 为 null 才是 normal。
   dev/build 依赖不链接进二进制，不产生分发义务。 */

function cargoPackages() {
  const raw = execFileSync('cargo', ['metadata', '--format-version', '1'], {
    cwd: path.join(root, 'src-tauri'),
    maxBuffer: 1 << 28,
    encoding: 'utf8',
  })
  const meta = JSON.parse(raw)
  const byId = new Map(meta.packages.map((p) => [p.id, p]))
  const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]))

  const reached = new Set()
  const queue = [meta.resolve.root]
  while (queue.length) {
    const id = queue.shift()
    if (reached.has(id)) continue
    reached.add(id)
    for (const dep of nodes.get(id)?.deps ?? []) {
      if (dep.dep_kinds.some((k) => k.kind === null)) queue.push(dep.pkg)
    }
  }
  reached.delete(meta.resolve.root)

  return [...reached]
    .map((id) => byId.get(id))
    .filter(Boolean)
    .map((pkg) => ({
      name: pkg.name,
      version: pkg.version,
      license: pkg.license ?? pkg.license_file ?? '(not declared)',
      repository: pkg.repository ?? null,
      texts: licenseTexts(path.dirname(pkg.manifest_path)),
    }))
    .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version))
}

/* ── npm ─────────────────────────────────────────────────
   package-lock v3 的 packages 表已经标好了 dev/optional，比跑 npm ls 稳。 */

function npmPackages() {
  const lock = JSON.parse(fs.readFileSync(path.join(root, 'package-lock.json'), 'utf8'))
  return Object.entries(lock.packages ?? {})
    .filter(([key, meta]) => key.startsWith('node_modules/') && !meta.dev && !meta.devOptional)
    .map(([key, meta]) => {
      const dir = path.join(root, key)
      let declared = meta.license
      if (!declared) {
        try {
          declared = JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8')).license
        } catch {
          declared = null
        }
      }
      return {
        name: key.slice('node_modules/'.length),
        version: meta.version,
        license: typeof declared === 'string' ? declared : '(not declared)',
        texts: licenseTexts(dir),
      }
    })
    .sort((a, b) => a.name.localeCompare(b.name))
}

/* ── 输出 ───────────────────────────────────────────────── */

function table(rows) {
  return [
    '| Package | Version | License |',
    '| --- | --- | --- |',
    ...rows.map((r) => `| ${r.name} | ${r.version} | ${r.license} |`),
  ].join('\n')
}

function main() {
  const cargo = cargoPackages()
  const npm = npmPackages()

  // 文本 → 共享它的包。同一份 Apache-2.0 全文只落一次。
  const texts = new Map()
  for (const pkg of [...npm, ...cargo]) {
    for (const { text } of pkg.texts) {
      if (!texts.has(text)) texts.set(text, new Set())
      texts.get(text).add(`${pkg.name} ${pkg.version}`)
    }
  }

  const missing = [...npm, ...cargo].filter((p) => p.texts.length === 0)

  const out = []
  out.push('# Third-Party Notices')
  out.push('')
  out.push('Tsubasa Redis Manager is distributed under the MIT License (see `LICENSE`).')
  out.push('It bundles third-party components that remain under their own licenses.')
  out.push('')
  out.push('The license and copyright texts these components require to be reproduced are')
  out.push('in [THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt). This file is the')
  out.push('inventory; that file is the notice.')
  out.push('')
  out.push('Both are generated — run `node scripts/dev/gen-notices.mjs` after any dependency')
  out.push('change rather than editing them by hand. They cover only what is actually')
  out.push('shipped: Cargo dependencies reachable through normal (non-dev, non-build) edges,')
  out.push('and npm dependencies that are not dev-only. Build-time tooling is not linked')
  out.push('into the binary and carries no distribution duty.')
  out.push('')
  out.push('Dependencies for every platform Cargo resolves are listed, not only the host')
  out.push('one, so a macOS or Linux build is covered by the same inventory.')
  out.push('')
  out.push(`Covered: ${npm.length} npm packages, ${cargo.length} Rust crates.`)
  out.push('')
  out.push('## npm packages')
  out.push('')
  out.push(table(npm))
  out.push('')
  out.push('## Rust crates')
  out.push('')
  out.push(table(cargo))
  out.push('')

  if (missing.length) {
    out.push('## Packages shipping no license file')
    out.push('')
    out.push('These declare a license in their manifest but ship no license text inside the')
    out.push('published package. The SPDX identifier in the tables above governs, and the')
    out.push('standard text for each of those identifiers already appears in')
    out.push('THIRD_PARTY_LICENSES.txt by way of the other packages using it.')
    out.push('')
    out.push(table(missing))
    out.push('')
  }

  out.push(MANUAL_NOTES)

  const notices = []
  notices.push('THIRD-PARTY LICENSE TEXTS')
  notices.push('')
  notices.push('Reproduced verbatim from the packages bundled with Tsubasa Redis Manager.')
  notices.push('See THIRD_PARTY_NOTICES.md for the package inventory and versions.')
  notices.push('')
  notices.push('Identical texts appear once, with every package sharing them named above the')
  notices.push('text. Per-package copyright lines are therefore preserved as written.')
  notices.push('')

  let index = 0
  for (const [text, users] of texts) {
    index += 1
    const names = [...users].sort()
    notices.push('='.repeat(78))
    notices.push(`${index}. ${names.join(', ')}`)
    notices.push('='.repeat(78))
    notices.push('')
    notices.push(text)
    notices.push('')
  }

  const inventory = path.join(root, 'THIRD_PARTY_NOTICES.md')
  const licenses = path.join(root, 'THIRD_PARTY_LICENSES.txt')
  fs.writeFileSync(inventory, out.join('\n'), 'utf8')
  fs.writeFileSync(licenses, notices.join('\n'), 'utf8')

  const kb = (file) => (fs.statSync(file).size / 1024).toFixed(0)
  console.log(`wrote THIRD_PARTY_NOTICES.md (${kb(inventory)} KB)`)
  console.log(`wrote THIRD_PARTY_LICENSES.txt (${kb(licenses)} KB)`)
  console.log(`  npm: ${npm.length}, cargo: ${cargo.length}, license texts: ${texts.size}`)
  if (missing.length) {
    console.log(`  no license file shipped: ${missing.length} packages`)
  }
}

main()
