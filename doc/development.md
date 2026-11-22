# 开发

覆盖：`package.json` 脚本、`scripts/dev/`、`src-tauri/tests/`、各处 `*.test.ts`、`.github/workflows/`、`CHANGELOG.md`

## 常用命令

```powershell
npm run dev          # vite dev server（127.0.0.1:1420）
npm run tauri:dev    # 完整桌面应用
npm run build        # vue-tsc -b && vite build，类型错误会让构建失败
npm run check        # build + clippy + cargo test（不需要 Redis）
cargo test --manifest-path src-tauri/Cargo.toml

./scripts/dev/e2e.ps1        # 起本地 Redis（单机 + 集群）跑 e2e，跑完保留环境
./scripts/dev/e2e.ps1 -Down  # 拆掉
```

## e2e

e2e 测试默认 `#[ignore]`，`npm run check` 必须在没有 Redis 的机器上也能跑。
单元测试喂的是手工构造的 `redis::Value`，而真正会出错的是真实服务端回什么形状：
分页端点、集群路由、字节保真。改动 `services/values.rs` 或连接层之后跑一次 e2e。

`e2e.ps1` 除了 Redis 7 单机、集群、带密码实例与 Redis 6，还起一组能力矩阵：Redis 5、Valkey 8 / 9、
KeyDB、Dragonfly（端口与镜像以脚本里的 `$Matrix` 为准），供 `tests/redis_capabilities.rs` 用。
身份字段与能力结论只能对着真实服务端定，见 [backend/capabilities](backend/capabilities.md)。
ACL 场景的用户由测试自己用 `ACL SETUSER … reset` 建、用完删。

## 测试分层与夹具

| 层 | 跑法 | 管什么 |
|---|---|---|
| 前端单元测试 | `npm test`（`vitest run`），`*.test.ts` 与被测模块同目录 | 纯函数与 store：codec 解析器、解码链、key 路径、CLI 行编辑、会话 / 任务 / 克隆 store |
| Rust 单元测试 | `cargo test`，`#[cfg(test)]` 模块 | 命令拼装、形状转换、任务逻辑，喂手工构造的 `redis::Value` |
| e2e | `./scripts/dev/e2e.ps1`，`src-tauri/tests/` 里 `#[ignore]` 的用例 | 真实服务端的形状，见上一节 |

`npm run check` 跑前两层。codec 解析器是纯函数、纯字节，PHP 的字节长度语义、protobuf 的 varint 这类东西
**不测必错**，所以前端测试是必需的而不是可选的。

`vitest` 与 `@msgpack/msgpack` 都是 devDependency（后者只在 `msgpack.test.ts` 里当独立实现的对照，
见 [frontend/codec-formats](frontend/codec-formats.md)「不用 @msgpack/msgpack 的五条理由」）。
`gen-notices.mjs` 的 npm 侧剔除 `dev` / `devOptional`——构建期工具不进产物，不产生分发义务——所以它们
**不进**许可清单。

**格式的事实要从产生这个格式的实现那里取，不能从自己的理解里取。** 手工构造的夹具会照着实现者的同一个
误解写出来，测试照绿。所以：

- pickle 的夹具是真实 CPython `pickle.dumps()` 吐出的字节，期望值取自同一次运行的 `repr(obj)` 与
  `pickletools.dis`，不是照着实现反推的。反例就在 opcode 表里：`\x90` 是 `ADDITEMS`、`\x91` 是
  `FROZENSET`，与名字的直觉顺序相反——照直觉写的解析器配照直觉写的夹具，测试照绿。
- Java 的夹具由真实 JDK 的 `ObjectOutputStream` 生成，生成器是 `scripts/dev/GenerateJavaCodecFixtures.java`，
  换 JDK 时可以重跑复核。只有损坏输入才由测试改字节构造。
- PHP 的 float 写法在真实 PHP 上实测（`docker run --rm php:{5.6,7.4,8.3}-cli`），见
  [frontend/codec-formats](frontend/codec-formats.md)「float.literal」。
- msgpack 拿 `@msgpack/msgpack` 当独立实现交叉验证。
- Redis 本身同理：单元测试喂的是手工构造的值，服务端真正回什么形状只有 e2e 说得准。

解析器比参考实现更严格的地方要写成用例并注明（例如 pickle 拒绝 `STOP` 之后的尾部字节，而 `pickle.loads`
不管），否则下一个人会照着参考实现把它「修」松。

## CI 与发布

- **CI**（`.github/workflows/ci.yml`）：推送与 PR 在 `windows-latest` 上分步跑 `npm run check` 的四段。
  不跑 e2e——e2e 要 Docker 起一组 Redis，合回 main 前在本地跑。
- **发布**（`.github/workflows/release.yml`）：在 main 上推 `v*` tag 触发。guard 校验 tag 落在 main 上、
  且与 `package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json` 三处版本号一致，
  然后在 Windows 上出 NSIS 安装包，发布为 GitHub Release（0.x 期间标为 pre-release）。
- 发版步骤：三处版本号一起改、把 `CHANGELOG.md` 的 `Unreleased` 落成新版本小节，合入 main 后打 tag 推送。
- `.gitattributes` 把工作流文件钉成 LF：`run:` 块里混进 CR，bash 会报 `$'\r': command not found`。

## 许可清单

新增运行时依赖后跑 `node scripts/dev/gen-notices.mjs`，重新生成
`THIRD_PARTY_NOTICES.md` 与 `THIRD_PARTY_LICENSES.txt`。两个文件是生成的，
只有 `MANUAL_NOTES` 那一节手写。devDependency 不进清单。

## 环境

本机 HTTP 代理会拦截对 `127.0.0.1` 的请求并回 503。用 curl 探测本地服务时加
`--noproxy '*'`，否则会把代理的错误当成服务的错误。
