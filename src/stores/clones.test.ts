import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { getCloneTask } from '@/api/redis'
import { useClonesStore } from './clones'
import type { CloneTaskSnapshot, TaskEventItem } from '@/types/ipc'

const mocks = vi.hoisted(() => ({
  subscribe: vi.fn(), notify: vi.fn(), open: vi.fn(), invalidate: vi.fn(), remember: vi.fn(),
  summaries: new Map(), sessions: new Map(),
}))
vi.mock('@/api/redis', () => ({ getCloneTask: vi.fn() }))
vi.mock('./tasks', () => ({ useTasksStore: () => ({ subscribe: mocks.subscribe, tasks: mocks.summaries }) }))
vi.mock('./tabs', () => ({ useTabsStore: () => ({ open: mocks.open }) }))
vi.mock('./ui', () => ({ useUiStore: () => ({ notify: mocks.notify }) }))
vi.mock('./sessions', () => ({ useSessionsStore: () => ({ sessions: mocks.sessions, invalidateKeyspace: mocks.invalidate, rememberKey: mocks.remember }) }))

const detail = (phase: CloneTaskSnapshot['phase']): CloneTaskSnapshot => ({
  taskId: 'job', sourceConnectionId: 'source', targetConnectionId: 'target', sourceName: 'Source', targetName: 'Target / db0',
  sourceKey: 'key', targetKey: 'copy', phase, keyType: 'string', bytes: 1024, errorCode: null, message: phase,
})
let emit: (items: TaskEventItem[]) => void
beforeEach(() => {
  vi.useFakeTimers()
  vi.clearAllMocks()
  setActivePinia(createPinia())
  mocks.summaries.clear()
  mocks.sessions.clear()
  mocks.sessions.set('target', { id: 'target', connectionKey: null, searchQuery: '', searchScope: null, pattern: '' })
  mocks.subscribe.mockImplementation((_id, sink) => { emit = sink; return vi.fn() })
})
afterEach(() => { vi.clearAllTimers(); vi.useRealTimers() })

describe('background clone lifecycle', () => {
  it('handles completion replayed before the start response without duplicate side effects', () => {
    const stop = vi.fn()
    mocks.subscribe.mockImplementation((_id, sink) => { sink([{ type: 'clone', snapshot: detail('completed') }]); return stop })
    const store = useClonesStore()
    store.track('job', true)
    store.track('job', true)
    expect(stop).toHaveBeenCalledOnce()
    expect(mocks.open).toHaveBeenCalledOnce()
    expect(mocks.invalidate).toHaveBeenCalledWith('target')
    expect(getCloneTask).not.toHaveBeenCalled()
  })
  it('recovers a lost terminal event by polling and clears the running status bar', async () => {
    vi.mocked(getCloneTask).mockResolvedValueOnce(detail('reading')).mockResolvedValueOnce(detail('completed'))
    const store = useClonesStore()
    store.track('job', false)
    await vi.advanceTimersByTimeAsync(0)
    expect(store.jobs.get('job')?.phase).toBe('reading')
    await vi.advanceTimersByTimeAsync(1000)
    expect(mocks.summaries.get('job').status).toBe('completed')
    expect(mocks.remember).toHaveBeenCalledWith('target', 'copy', 'string')
    expect(mocks.open).not.toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(4000)
    expect(mocks.summaries.has('job')).toBe(false)
    expect(store.jobs.has('job')).toBe(true)
  })
  it('does not regress to an older poll result or treat an uncertain write as success', async () => {
    let resolve!: (value: CloneTaskSnapshot) => void
    vi.mocked(getCloneTask).mockReturnValueOnce(new Promise(done => { resolve = done }))
    const store = useClonesStore()
    store.track('job', true)
    emit([{ type: 'clone', snapshot: detail('restoring') }])
    resolve(detail('reading'))
    await vi.advanceTimersByTimeAsync(0)
    expect(store.jobs.get('job')?.phase).toBe('restoring')
    emit([{ type: 'clone', snapshot: { ...detail('uncertain'), errorCode: 'redis.key_outcome_unknown' } }])
    expect(mocks.open).not.toHaveBeenCalled()
    expect(mocks.invalidate).not.toHaveBeenCalled()
    expect(mocks.notify).toHaveBeenCalledWith('danger', expect.any(String), 'copy')
    emit([{ type: 'clone', snapshot: detail('reading') }])
    expect(store.jobs.get('job')?.phase).toBe('uncertain')
  })
})
