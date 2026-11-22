import { describe, expect, it } from 'vitest'
import {
  buildKeyTree,
  flattenTree,
  isPrefixCovered,
  matchPatternFor,
  scopedSearchPattern,
} from './keyTree'
import type { KeyEntry } from '@/types/ipc'

function entry(key: string): KeyEntry {
  return { key, keyType: 'string', ttl: null }
}

describe('matchPatternFor', () => {
  it('前缀加分隔符再加星号', () => {
    expect(matchPatternFor('cache:page', ':')).toBe('cache:page:*')
  })

  it('空前缀是整个 DB', () => {
    expect(matchPatternFor('', ':')).toBe('*')
  })

  it('分隔符为空时退化成前缀匹配', () => {
    expect(matchPatternFor('cache', '')).toBe('cache*')
  })

  /*
   * 这一组是批量删除的安全前提，不是锦上添花。
   *
   * 不转义时 `evil[ab]:*` 里的 `[ab]` 是字符类：匹配不到用户右键的那一片，
   * 反而命中 `evila:1`。右键 → 批量删除会因此删掉一个无关的 key。
   */
  describe('glob 元字符必须转义', () => {
    it('方括号不再是字符类', () => {
      expect(matchPatternFor('evil[ab]', ':')).toBe('evil\\[ab\\]:*')
    })

    it('前缀里的星号与问号只代表它们自己', () => {
      expect(matchPatternFor('a*b', ':')).toBe('a\\*b:*')
      expect(matchPatternFor('v?', ':')).toBe('v\\?:*')
    })

    it('反斜杠自己也要转义，否则会吃掉后面那个字符', () => {
      expect(matchPatternFor('a\\b', ':')).toBe('a\\\\b:*')
    })

    it('分隔符同样要过一遍 —— 它也可能是元字符', () => {
      expect(matchPatternFor('cache', '?')).toBe('cache\\?*')
    })

    it('末尾的星号是我们自己加的，不受转义影响', () => {
      expect(matchPatternFor('cache:page', ':').endsWith(':*')).toBe(true)
    })
  })

  /*
   * 转义之后模式的语义**就是**「以这个字面前缀开头」。删完裁树时
   * 前端一个 startsWith 就精确等价，不必再实现一份 glob 匹配器。
   */
  it('右键拿到的段落路径就是字面前缀，与模式同义', () => {
    const tree = buildKeyTree([entry('evil[ab]:1'), entry('evila:1')], ':')
    const rows = flattenTree(tree, { expanded: new Set(), compact: true, separator: ':' })
    // 紧凑合并把 `evil[ab]` 与它唯一的孩子并成一行，中间段只在右键时才有身份 ——
    // 菜单拿的是 segments[i].path，与这里取的是同一个东西
    const segment = rows
      .flatMap((row) => row.segments)
      .find((item) => item.path === 'evil[ab]')

    expect(segment).toBeDefined()
    expect(matchPatternFor(segment!.path, ':')).toBe('evil\\[ab\\]:*')
    expect('evil[ab]:1'.startsWith(`${segment!.path}:`)).toBe(true)
    expect('evila:1'.startsWith(`${segment!.path}:`)).toBe(false)
  })
})

/*
 * 「在此搜索」的模式拼装（doc/frontend/key-tree.md「文件夹内搜索与前缀权威」）。两个既有决策在这里汇合，缺一不可：
 * 前缀来自树、是字面路径，必须转义；用户词是 glob，**不转义** ——
 * 同一个词在全局搜索框与前缀作用域下语义分裂，比元字符伤到新手更糟。
 */
describe('scopedSearchPattern', () => {
  it('前缀 + 分隔符在前，包裹后的词在后', () => {
    expect(scopedSearchPattern('user', ':', 'foo')).toBe('user:*foo*')
  })

  it('词里已有 * 就原样使用，与全局搜索同一条包裹规则', () => {
    expect(scopedSearchPattern('user', ':', 'foo*')).toBe('user:foo*')
  })

  it('词为空即「扫描此前缀」，与 matchPatternFor 完全一致', () => {
    expect(scopedSearchPattern('user', ':', '')).toBe(matchPatternFor('user', ':'))
    expect(scopedSearchPattern('user', ':', '   ')).toBe('user:*')
  })

  it('前缀里的元字符转义，词里的不转义', () => {
    expect(scopedSearchPattern('evil[ab]', ':', 'v?')).toBe('evil\\[ab\\]:*v?*')
    expect(scopedSearchPattern('a*b', ':', 'x[1]')).toBe('a\\*b:*x[1]*')
  })

  it('分隔符与前缀同属字面部分，一样要转义', () => {
    expect(scopedSearchPattern('cache', '?', 'x')).toBe('cache\\?*x*')
  })

  it('无前缀时退化成全局搜索的包裹规则', () => {
    expect(scopedSearchPattern('', ':', 'foo')).toBe('*foo*')
    expect(scopedSearchPattern('', ':', 'fo*o')).toBe('fo*o')
  })
})

/*
 * 前缀权威集的包含关系（doc/frontend/key-tree.md「前缀权威」）。集合按完整前缀存（不是行索引），
 * 判定必须对子前缀成立：`user` 扫到底了，`user:profile` 是它子树的子树。
 */
describe('isPrefixCovered', () => {
  const set = new Set(['user', 'evil[ab]'])

  it('前缀本身在集合里', () => {
    expect(isPrefixCovered(set, 'user', ':')).toBe(true)
  })

  it('子前缀与更深的子孙都被覆盖', () => {
    expect(isPrefixCovered(set, 'user:profile', ':')).toBe(true)
    expect(isPrefixCovered(set, 'user:profile:1001', ':')).toBe(true)
  })

  it('只认整段边界：user 不覆盖 userx', () => {
    expect(isPrefixCovered(set, 'userx', ':')).toBe(false)
    expect(isPrefixCovered(set, 'userx:1', ':')).toBe(false)
  })

  it('祖先不因子孙权威而权威 —— 方向不能反', () => {
    expect(isPrefixCovered(new Set(['user:profile']), 'user', ':')).toBe(false)
  })

  it('集合里的前缀按字面比较，元字符没有特殊含义', () => {
    expect(isPrefixCovered(set, 'evil[ab]:1', ':')).toBe(true)
    expect(isPrefixCovered(set, 'evila:1', ':')).toBe(false)
  })

  it('空集合、空路径都不覆盖', () => {
    expect(isPrefixCovered(new Set<string>(), 'user:1', ':')).toBe(false)
    expect(isPrefixCovered(set, '', ':')).toBe(false)
  })

  it('空分隔符（列表模式）只认完全相等', () => {
    expect(isPrefixCovered(new Set(['user']), 'user', '')).toBe(true)
    expect(isPrefixCovered(new Set(['user']), 'user:1', '')).toBe(false)
  })

  it('多字符分隔符按整串剥段', () => {
    expect(isPrefixCovered(new Set(['a']), 'a::b::c', '::')).toBe(true)
    expect(isPrefixCovered(new Set(['a:']), 'a::b', '::')).toBe(false)
  })
})
