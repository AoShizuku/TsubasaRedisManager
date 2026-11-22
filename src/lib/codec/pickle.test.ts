import { describe, expect, it, vi } from 'vitest'
import { decodeValue, decodeWithReadonly, isImplemented, unimplementedReason } from './index'
import { pickleCodec } from './pickle'
import { renderNode } from './render'
import type { Node } from './types'
import { base64FromBytes } from '@/lib/redisText'
import type { RedisText } from '@/types/ipc'

/* `./index` 静态 import 了 `@/api/native`，那条链一路通到 @tauri-apps/api。
   pickle 这一侧一次 IPC 都不该发，所以整个模块换成假的，顺带钉住"没发过"。 */
vi.mock('@/api/native', () => ({
  codecDecode: vi.fn(),
  codecEncode: vi.fn(),
  codecDetect: vi.fn(),
}))

/**
 * ## 夹具全部来自真实的 `pickle.dumps`
 *
 * 每一段 hex 都是 CPython 3.14 吐出来的字节，期望值来自同一次运行的
 * `repr(obj)` 与 `pickletools.dis` —— **不是**照着本实现反推的。
 *
 * 这一条不是形式主义。写这个解析器时 `\x90` / `\x91` 被写反了
 * （ADDITEMS 与 FROZENSET），手工构造的夹具会照着同一个误解写出来、测试照绿；
 * 是 `pickletools.dis` 的输出把它顶出来的。CLAUDE.md 里"真正会出错的是
 * 真实服务端回什么形状"说的是同一件事，只是这里的"服务端"是 CPython。
 *
 * 生成方式（`pickle.dumps(obj, protocol=N).hex()`）记在每组用例的注释里，
 * 要补夹具照着加就行。
 */
function pick(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2)
  for (let i = 0; i < out.length; i += 1) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16)
  return out
}

function decode(hex: string): Node {
  return pickleCodec.decode(pick(hex))
}

function fails(hex: string): void {
  expect(() => decode(hex)).toThrow()
}

function str(v: string): Node {
  return { t: 'str', v }
}

function int(v: number | bigint): Node {
  return { t: 'int', v: BigInt(v) }
}

/* ── 基本类型，跨 protocol ────────────────────────────── */

describe('基本类型', () => {
  // pickle.dumps(None, protocol=0/2/4/5)
  it('None 在每个 protocol 下都是 null', () => {
    expect(decode('4e2e')).toEqual({ t: 'null' }) // protocol 0/1，没有 PROTO 前缀
    expect(decode('80024e2e')).toEqual({ t: 'null' })
    expect(decode('80044e2e')).toEqual({ t: 'null' })
    expect(decode('80054e2e')).toEqual({ t: 'null' })
  })

  /**
   * protocol 0 的 `I01\n` / `I00\n` 是 True / False，**不是**整数 1 和 0。
   * 解成 int 会把类型悄悄改掉，而屏幕上 `i64 1` 与 `true` 是两回事。
   */
  it('protocol 0 的 I01 是 True 而不是整数 1', () => {
    expect(decode('4930310a2e')).toEqual({ t: 'bool', v: true })
    expect(decode('4930300a2e')).toEqual({ t: 'bool', v: false })
    expect(decode('8002882e')).toEqual({ t: 'bool', v: true })
    expect(decode('8002892e')).toEqual({ t: 'bool', v: false })
  })

  it('整数：小的、负的、超出 int64 的', () => {
    expect(decode('4934320a2e')).toEqual(int(42)) // I42\n
    expect(decode('80044b2a2e')).toEqual(int(42)) // BININT1
    expect(decode('80049506000000000000004affffffff2e')).toEqual(int(-1)) // BININT
    // LONG1，超出 int64 —— `Node.int` 是 bigint 就是为了这种值
    expect(decode('8004950c000000000000008a09d20a1feb8ca954ab002e')).toEqual(
      int(12345678901234567890n),
    )
    expect(decode('8004950c000000000000008a092ef5e0147356ab54ff2e')).toEqual(
      int(-12345678901234567890n),
    )
  })

  it('浮点：文本的与二进制的', () => {
    expect(decode('46302e310a2e')).toEqual({ t: 'float', v: 0.1, bits: 64 }) // F0.1\n
    // BINFLOAT 是**大端**，跟其余定宽字段的小端相反
    expect(decode('8004950a00000000000000473fb999999999999a2e')).toEqual({
      t: 'float',
      v: 0.1,
      bits: 64,
    })
  })

  /**
   * `inf` / `nan` 必须特判：JS 的 `Number('inf')` 是 `NaN`，照着 `Number()` 走
   * 会把正无穷悄悄变成 NaN —— 一次静默的值改写，而且看不出来。
   */
  it('protocol 0 的 inf / nan 不能走 Number()', () => {
    expect(decode('46696e660a2e')).toEqual({
      t: 'float',
      v: Number.POSITIVE_INFINITY,
      bits: 64,
    })
    expect(decode('462d696e660a2e')).toEqual({
      t: 'float',
      v: Number.NEGATIVE_INFINITY,
      bits: 64,
    })
    const nan = decode('466e616e0a2e')
    expect(nan.t === 'float' && Number.isNaN(nan.v)).toBe(true)
  })

  it('字符串与字节是两个类型', () => {
    // SHORT_BINUNICODE 'hello'
    expect(decode('80049509000000000000008c0568656c6c6f942e')).toEqual(str('hello'))
    // SHORT_BINBYTES b'\x00\x01\xff\xfe' —— 非 UTF-8，必须留在 bytes 里
    expect(decode('800495080000000000000043040001fffe942e')).toEqual({
      t: 'bytes',
      v: Uint8Array.from([0x00, 0x01, 0xff, 0xfe]),
    })
  })

  it('非 ASCII 的 str 按 UTF-8 解', () => {
    // pickle.dumps('héllo·世界', protocol=4)
    expect(decode('80049512000000000000008c0e68c3a96c6c6fc2b7e4b896e7958c942e')).toEqual(
      str('héllo·世界'),
    )
  })

  it('protocol 5 的 bytearray 也是一段字节', () => {
    expect(decode('8005950d000000000000009602000000000000000102942e')).toEqual({
      t: 'bytes',
      v: Uint8Array.from([1, 2]),
    })
  })
})

/* ── protocol 0 的文本转义：两套编码，不能混 ───────────── */

describe('protocol 0 的文本参数', () => {
  /**
   * `UNICODE` 用的是 `raw-unicode-escape`：`\uXXXX` 按转义解，**其余字节按
   * latin-1**，而且它**不**处理 `\n` 这类转义（"raw" 就是这个意思）。
   *
   * 这段夹具正好两样都有：换行被写成六个字节的转义（反斜杠 u 加四位十六进制），
   * 而制表符是**字面的 0x09 字节**。拿 `STRING` 那套 repr 转义来凑合的话，
   * 0x09 碰巧对，那个六字节转义却会被当成反斜杠加一个字母 u。
   */
  it('UNICODE 是 raw-unicode-escape，不是 repr 转义', () => {
    // pickle.dumps("a\nb\tc\\d'e\"f", protocol=0)
    expect(decode('56615c75303030616209635c753030356364276522660a70300a2e')).toEqual(
      str('a\nb\tc\\d\'e"f'),
    )
  })

  it('UNICODE 里 latin-1 字节与 \\uXXXX 混排', () => {
    // pickle.dumps('é世', protocol=0) —— 0xe9 是 latin-1 的 é，世走 世
    expect(decode('56e95c75346531360a70300a2e')).toEqual(str('é世'))
  })

  /**
   * `STRING`（Python 2 的 str）走的是另一套：`repr` 风格的引号串。
   * `\0` 必须走八进制那条路 —— `\012` 是一个字节 0x0a，把 `\0` 单独拦下来
   * 会把它解成一个 NUL 再跟两个字面数字 `1` `2`。
   */
  it('STRING 的八进制转义按最多三位吃', () => {
    // S'|\012|'\n. —— Python 解出来是 '|\n|'，那三位八进制是一个字节
    expect(decode('53277c5c3031327c270a2e')).toEqual(str('|\n|'))
  })

  it('STRING 的 \\xNN 与常见单字符转义', () => {
    expect(decode("532761785c7831465c7462270a2e")).toEqual(str('ax\x1f\tb'))
  })

  it('STRING 必须带引号', () => {
    fails('536162630a2e') // Sabc\n. —— 没引号
  })
})

/* ── 容器 ─────────────────────────────────────────────── */

describe('容器', () => {
  it('list 走 arr，两种 protocol 都是', () => {
    // protocol 0：MARK LIST + 逐个 APPEND
    expect(decode('286c70300a49310a6149320a6149330a612e')).toEqual({
      t: 'arr',
      v: [int(1), int(2), int(3)],
    })
    // protocol 4：EMPTY_LIST + APPENDS
    expect(decode('8004950b000000000000005d94284b014b024b03652e')).toEqual({
      t: 'arr',
      v: [int(1), int(2), int(3)],
    })
  })

  /**
   * **tuple 不是 list。** 挤进 `arr` 之后屏幕上 `[1, 2, 3]` 分不出是哪一种，
   * 而 Python 那边它俩是两个类型 —— 这跟 int64 精度、非字符串 map key 是同一类损失
   * （doc/frontend/codec-formats.md「pickle」）。
   */
  it('tuple / set / frozenset 各自保住自己的类型', () => {
    expect(decode('80049509000000000000004b014b024b0387942e')).toEqual({
      t: 'seq',
      cls: 'tuple',
      v: [int(1), int(2), int(3)],
    })
    expect(decode('80049505000000000000004b0185942e')).toEqual({
      t: 'seq',
      cls: 'tuple',
      v: [int(1)],
    })
    expect(decode('8004950b000000000000008f94284b014b024b03902e')).toEqual({
      t: 'seq',
      cls: 'set',
      v: [int(1), int(2), int(3)],
    })
    expect(decode('8004950800000000000000284b014b0291942e')).toEqual({
      t: 'seq',
      cls: 'frozenset',
      v: [int(1), int(2)],
    })
  })

  /**
   * `\x90` 是 ADDITEMS、`\x91` 是 FROZENSET —— 顺序与名字的直觉相反。
   * 写反了的话上面那两条会互相解成对方，而**空集合**这一条会直接崩：
   * 一个 EMPTY_SET 后面跟 MARK + FROZENSET，两个 opcode 都在里面。
   */
  it('空容器全都摆在一起时不会串味', () => {
    // pickle.dumps(([], {}, (), set(), frozenset()), protocol=4)
    expect(decode('8004950e00000000000000285d947d94298f9428919474942e')).toEqual({
      t: 'seq',
      cls: 'tuple',
      v: [
        { t: 'arr', v: [] },
        { t: 'map', v: [] },
        { t: 'seq', cls: 'tuple', v: [] },
        { t: 'seq', cls: 'set', v: [] },
        { t: 'seq', cls: 'frozenset', v: [] },
      ],
    })
  })

  it('dict 的 key 保住类型与顺序', () => {
    // pickle.dumps({1: 'x', 'b': 2}, protocol=4) —— int 键与 str 键混排
    expect(decode('80049511000000000000007d94284b018c0178948c0162944b02752e')).toEqual({
      t: 'map',
      v: [
        [int(1), str('x')],
        [str('b'), int(2)],
      ],
    })
    // protocol 0 的 DICT + 逐个 SETITEM
    expect(decode('286470300a56610a70310a49310a7356620a70320a49320a732e')).toEqual({
      t: 'map',
      v: [
        [str('a'), int(1)],
        [str('b'), int(2)],
      ],
    })
  })

  it('嵌套结构', () => {
    // pickle.dumps({'users': [{'id': 1, 'tags': ('a', 'b')}]}, protocol=4)
    const hex =
      '8004952b000000000000007d948c057573657273945d947d94288c026964944b018c0474616773948c0161948c01629486947561732e'
    expect(decode(hex)).toEqual({
      t: 'map',
      v: [
        [
          str('users'),
          {
            t: 'arr',
            v: [
              {
                t: 'map',
                v: [
                  [str('id'), int(1)],
                  [str('tags'), { t: 'seq', cls: 'tuple', v: [str('a'), str('b')] }],
                ],
              },
            ],
          },
        ],
      ],
    })
  })
})

/* ── memo：引用不展开 ─────────────────────────────────── */

describe('memo 引用', () => {
  /**
   * 展开成共享子树会让产物不再是一棵树 —— `a = [大对象]; [a] * 100` 把展示撑爆，
   * 自引用容器直接就是个环。判据与 `php.ts` 处理 `R:` / `r:` 完全一样。
   *
   * 而且标记**信息更多**：`&memo:1` 说的是"这里和前面是同一个对象"，
   * 展开成副本反而会让人以为是两个恰好相等的值。
   */
  it('BINGET 记成引用标记，不展开', () => {
    // shared = ['shared']; pickle.dumps([shared, shared], protocol=4)
    const decoded = decode('80049513000000000000005d94285d948c0673686172656494616801652e')
    expect(decoded).toEqual({
      t: 'arr',
      v: [
        { t: 'arr', v: [str('shared')] },
        { t: 'call', fn: '&memo:1', v: null },
      ],
    })
  })

  it('protocol 0 的 GET 走同一条路', () => {
    const decoded = decode('286c70300a286c70310a567368617265640a70320a616167310a612e')
    expect(decoded).toEqual({
      t: 'arr',
      v: [
        { t: 'arr', v: [str('shared')] },
        { t: 'call', fn: '&memo:1', v: null },
      ],
    })
  })

  /** 取不到就抛错。一个 `undefined` 混进树里比失败难查得多。 */
  it('取一个不存在的 memo 编号是错', () => {
    fails('8004680a2e') // BINGET 10，memo 是空的
  })
})

/* ── 构造调用：本功能的安全价值所在 ───────────────────── */

describe('调用点', () => {
  /**
   * `GLOBAL` 解出名字但**没有被调用**，`REDUCE` 才是那次调用 ——
   * 两者在 pickle 里是两串不同的字节，而只有后者会执行。
   */
  it('REDUCE 解成一次调用，参数摆出来', () => {
    // pickle.dumps(datetime.datetime(2024, 3, 5, 6, 7, 8), protocol=4)
    const hex =
      '8004952a000000000000008c086461746574696d65948c086461746574696d65949394430a07e8030506070800000094859452942e'
    expect(decode(hex)).toEqual({
      t: 'call',
      fn: 'datetime.datetime',
      v: [{ t: 'bytes', v: Uint8Array.from([0x07, 0xe8, 3, 5, 6, 7, 8, 0, 0, 0]) }],
    })
  })

  /**
   * **这就是那个危险的东西，而我们只是把它摆出来。**
   *
   * 这段字节是 `os.system('echo pwned')` 的 payload（用 `__reduce__` 生成，
   * `dumps` 本身不执行它）。用 Python 打开它就中招；本工具解出来是一个
   * `call` 节点，一次都没有去解析那个名字、加载模块或调用什么。
   *
   * doc/backend/codec.md「Readonly 安全边界」的论证就落在这一条上：只解析不执行的查看器，是这个格式**唯一安全**
   * 的查看方式 —— 替代方案是用户自己去跑 `pickle.loads()`。
   */
  it('恶意 payload 解得出来，而且只是数据', () => {
    // Windows 上 os.system 的模块是 nt；Linux 上会是 posix，字节里写的是哪个就是哪个
    const hex =
      '80049522000000000000008c026e74948c0673797374656d9493948c0a6563686f2070776e656494859452942e'
    expect(decode(hex)).toEqual({
      t: 'call',
      fn: 'nt.system',
      v: [str('echo pwned')],
    })

    // 屏幕上它长这样 —— 调用语法本身就是标注，见 render.ts 的 renderCall
    expect(renderNode(decode(hex))).toContain('nt.system')
    expect(renderNode(decode(hex))).toContain('"echo pwned"')
  })

  it('protocol 0 的 GLOBAL + TUPLE + REDUCE 是同一件事', () => {
    const hex = '636e740a73797374656d0a70300a28566563686f2070776e65640a70310a7470320a5270330a2e'
    expect(decode(hex)).toEqual({ t: 'call', fn: 'nt.system', v: [str('echo pwned')] })
  })

  it('builtins.eval 的 payload 也一样', () => {
    const hex =
      '80049539000000000000008c086275696c74696e73948c046576616c9493948c1d5f5f696d706f72745f5f28276f7327292e6c69737464697228272e272994859452942e'
    expect(decode(hex)).toEqual({
      t: 'call',
      fn: 'builtins.eval',
      v: [str("__import__('os').listdir('.')")],
    })
  })

  /**
   * `NEWOBJ` + `BUILD`：一个普通的自定义类。state 是**构造之后**补上的，
   * 跟构造参数是两回事，所以它有自己的字段。
   */
  it('NEWOBJ 加 BUILD：类名、参数、state 三样都在', () => {
    // class Point: def __init__(self, x, y) —— pickle.dumps(Point(1, 2), protocol=4)
    const hex =
      '8004952a000000000000008c085f5f6d61696e5f5f948c05506f696e749493942981947d94288c0178944b018c0179944b0275622e'
    expect(decode(hex)).toEqual({
      t: 'call',
      fn: '__main__.Point',
      v: [],
      state: {
        t: 'map',
        v: [
          [str('x'), int(1)],
          [str('y'), int(2)],
        ],
      },
    })
  })

  /**
   * `SETITEMS` 落在一个 `REDUCE` 出来的对象上 —— 这不是边角情况，
   * `collections.OrderedDict` 就是这么写出来的。塞进去的内容归 `data`，
   * 与 `state`（`__dict__`）分开。
   */
  it('REDUCE 出来的对象后面跟 SETITEMS', () => {
    const hex =
      '80049530000000000000008c0b636f6c6c656374696f6e73948c0b4f72646572656444696374949394295294288c0161944b018c0162944b02752e'
    expect(decode(hex)).toEqual({
      t: 'call',
      fn: 'collections.OrderedDict',
      v: [],
      data: {
        t: 'map',
        v: [
          [str('a'), int(1)],
          [str('b'), int(2)],
        ],
      },
    })
  })

  it('REDUCE 出来的对象后面跟 APPENDS', () => {
    // pickle.dumps(collections.deque([1, 2, 3]), protocol=4)
    const hex =
      '80049524000000000000008c0b636f6c6c656374696f6e73948c056465717565949394295294284b014b024b03652e'
    expect(decode(hex)).toEqual({
      t: 'call',
      fn: 'collections.deque',
      v: [],
      data: { t: 'arr', v: [int(1), int(2), int(3)] },
    })
  })

  it('protocol 2 的 GLOBAL + NEWOBJ + BUILD，两个实例共享类', () => {
    // pickle.dumps([Point(1, 2), Point(3, 4)], protocol=2)
    const hex =
      '80025d710028635f5f6d61696e5f5f0a506f696e740a7101298171027d71032858010000007871044b0158010000007971054b0275626801298171067d71072868044b0368054b047562652e'
    const decoded = decode(hex)
    expect(decoded.t).toBe('arr')
    if (decoded.t !== 'arr') return
    expect(decoded.v).toHaveLength(2)
    expect(decoded.v[0]).toMatchObject({ t: 'call', fn: '__main__.Point' })
    // 第二个实例的类是 BINGET 拿回来的 —— 引用标记，不是重复的类名
    expect(decoded.v[1]).toMatchObject({ t: 'call', fn: '&memo:1' })
  })

  it('BUILD 落在不是对象的东西上要抛错', () => {
    // EMPTY_DICT, EMPTY_DICT, BUILD, STOP —— Python 自己 unpickle 也会失败
    fails('80047d947d94622e')
  })
})

/* ── 消费完整性：没有往返自检，这是唯一的机械保证 ─────── */

describe('消费完整性', () => {
  /**
   * CPython 的 `pickle.loads` 读到 `STOP` 就返回，尾部垃圾它不管。这里拒绝，
   * 理由与 `php.ts` 一样：`detectCodec` 拿"decode 抛不抛错"当自动识别的判据，
   * 宽松的解析器会把任何以 `.` 结尾的垃圾也认成 pickle。
   */
  it('STOP 之后不能有剩余字节', () => {
    expect(() => decode('80044e2e00')).toThrow(/还剩 1 个字节/)
  })

  /** 比"读到了 STOP"强得多：它抓的是 opcode 参数个数数错这类 bug。 */
  it('STOP 时栈上必须恰好一个值', () => {
    expect(() => decode('80044b014b022e')).toThrow(/栈上有 2 个值/) // 两个 BININT1
    expect(() => decode('80042e')).toThrow(/栈上有 0 个值/) // 空栈
  })

  it('没有 STOP 就是没解完', () => {
    fails('80044b01')
  })

  it('不认识的 opcode 要说清是哪个', () => {
    expect(() => decode('8004ff2e')).toThrow(/0xff/)
  })
})

/* ── 恶意输入：风险降级为资源耗尽，逐条挡 ─────────────── */

describe('恶意输入', () => {
  /**
   * `BINBYTES` 的长度字段是 4 字节，能声明 4 GB。先 `new Uint8Array(len)`
   * 再填的话，这一条当场就是一次 OOM —— 所以必须**先验剩余字节够不够**。
   */
  it('声明的长度超过剩余输入时立刻失败，不预分配', () => {
    // BINBYTES 长度 0xffffffff，后面什么都没有
    expect(() => decode('800442ffffffff2e')).toThrow(/只剩/)
    // BINBYTES8 声明 16 MiB（小端 8 字节），同样一个字节都没跟
    expect(() => decode('80048e00000001000000002e')).toThrow(/只剩/)
  })

  it('FRAME 的长度不能越过输入末尾', () => {
    expect(() => decode('800495ffffffffffffff7f4e2e')).toThrow(/只剩/)
  })

  /** `((((((…` 是一段合法但无限增长的输入。 */
  it('MARK 不能无限累积', () => {
    expect(() => decode('8004' + '28'.repeat(100_001) + '2e')).toThrow(/MARK 超过/)
  })

  /**
   * 嵌套深度。解析器本身是**迭代的栈机**，炸栈的不是它 ——
   * 是拿到产物之后递归遍历那棵树的人。所以上限设在解析这一侧，
   * 让"树太深"在这里就报出来，而不是变成某个下游的 RangeError。
   */
  it('嵌套超过上限要抛错，而且检查本身不能炸栈', () => {
    // EMPTY_LIST ×300 压栈，再 APPEND ×299 逐层套起来 —— 299 层嵌套。
    // 这段字节 CPython 自己 loads 是成功的（核对过），所以拒绝它的是我们的上限
    const depth = 300
    let build = '8004'
    for (let i = 0; i < depth; i += 1) build += '5d' // EMPTY_LIST
    for (let i = 0; i < depth - 1; i += 1) build += '61' // APPEND
    build += '2e'
    expect(() => decode(build)).toThrow(/嵌套超过/)
  })

  /**
   * protocol 5 的带外缓冲把数据放在字节流之外。Redis 里存不下这种值，
   * 遇到就是数据不对或者有人在试探 —— 要说清是哪一种，不能笼统报"解析失败"。
   */
  it('带外缓冲明确拒绝，并说清原因', () => {
    expect(() => decode('8005972e')).toThrow(/带外缓冲/)
    expect(() => decode('8005982e')).toThrow(/带外缓冲/)
  })

  it('protocol 号超出已知范围要拒绝', () => {
    expect(() => decode('80094e2e')).toThrow(/protocol 9/)
  })

  it('栈空时取值要报错而不是拿到 undefined', () => {
    fails('8004612e') // APPEND，栈上什么都没有
    fails('8004522e') // REDUCE，栈上什么都没有
  })

  it('取值不能越过 MARK', () => {
    fails('80044b0128612e') // BININT1, MARK, APPEND —— APPEND 要越过 MARK 取值
  })
})

/* ── 归属：只读那一侧的接线 ───────────────────────────── */

describe('只读编解码器的归属', () => {
  it('pickle 现在是"能跑"的', () => {
    expect(isImplemented('pickle')).toBe(true)
    expect(unimplementedReason('pickle')).toBeNull()
  })

  it('javaSerialize 也已经沿只读注册表落地', () => {
    expect(isImplemented('javaSerialize')).toBe(true)
    expect(unimplementedReason('javaSerialize')).toBeNull()
  })

  /**
   * **`kind: 'noEncoder'` 不是 `lossy`。** pickle 编得回去，是我们不编 ——
   * 套上"编不回原来的字节"就成了假话，让用户以为是自己的数据有问题。
   * 见 doc/frontend/codecs.md「Editable 的三种说法」。
   */
  it('产物是只读的，理由是格式而不是数据', () => {
    const decoded = decodeWithReadonly(pickleCodec, pick('80044e2e'), 'pickle')
    expect(decoded.kind).toBe('struct')
    if (decoded.kind !== 'struct') return
    expect(decoded.editable.ok).toBe(false)
    if (decoded.editable.ok) return
    expect(decoded.editable.kind).toBe('noEncoder')
    expect(decoded.editable.reason).toContain('只解码不编码')
  })

  it('decodeValue 按 conclusion 分派到只读那张表，一次 IPC 都不发', async () => {
    const source: RedisText = {
      text: null,
      base64: base64FromBytes(pick('80044b2a2e')),
      totalBytes: null,
    }
    const decoded = await decodeValue('pickle', source)
    expect(decoded.kind).toBe('struct')
    if (decoded.kind !== 'struct') return
    expect(decoded.root).toEqual(int(42))
  })
})

/* ── 自动识别：能解开远远不够 ─────────────────────────── */

describe('自动识别的前置约束', () => {
  /**
   * `N.` 两个字节就是一段合法 pickle（NONE + STOP）。光靠"能不能解开"当判据，
   * 任何以合法 opcode 开头、恰好以 `.` 结尾的短值都会被 pickle 认领 ——
   * 那正是 msgpack 不能进候选集的同一个毛病（doc/frontend/codecs.md「识别的三个维度」）。
   */
  it('protocol 0 / 1 解得开，但进不了自动识别', () => {
    expect(decode('4e2e')).toEqual({ t: 'null' }) // 手点解得开
    expect(pickleCodec.detectPrefix?.(pick('4e2e'))).toBe(false) // 但识别不认
  })

  it('protocol 2+ 的 PROTO 前缀才认', () => {
    expect(pickleCodec.detectPrefix?.(pick('80024e2e'))).toBe(true)
    expect(pickleCodec.detectPrefix?.(pick('80054e2e'))).toBe(true)
    expect(pickleCodec.detectPrefix?.(pick('80014e2e'))).toBe(false) // protocol 1 没有 PROTO
    expect(pickleCodec.detectPrefix?.(pick('80094e2e'))).toBe(false) // 超出已知范围
    expect(pickleCodec.detectPrefix?.(pick('80'))).toBe(false) // 太短
  })
})
