import type { InjectionKey } from 'vue'

/** 下拉菜单与右键菜单共用 MenuItem 的视觉，靠这个注入键区分实现。 */
export type MenuFlavor = 'dropdown' | 'context'
export const MENU_FLAVOR: InjectionKey<MenuFlavor> = Symbol('menu-flavor')
