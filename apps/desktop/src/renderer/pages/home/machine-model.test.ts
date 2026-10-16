import { describe, expect, it } from 'vitest'
import { CYCLE_MS, SOURCES, STAGES, STAGE_STARTS, STATIONS, machineAt, stepTo } from './machine-model'
import { homeMessages } from './home.i18n'

const mid = (i: number, run = 0) => run * CYCLE_MS + STAGE_STARTS[i] + STAGES[i].ms / 2
const idx = (id: string) => STAGES.findIndex((s) => s.id === id)

describe('machineAt', () => {
  it('walks every stage in order within one cycle', () => {
    expect(STAGES.map((_, i) => machineAt(mid(i)).stage)).toEqual(STAGES.map((s) => s.id))
  })

  it('rotates the trigger source run by run', () => {
    expect([0, 1, 2, 3].map((r) => machineAt(mid(0, r)).source)).toEqual(['entry', 'scheduler', 'comm', 'entry'])
  })

  it('keeps the packet visible at the closed gate and hides it inside stations', () => {
    const waiting = machineAt(mid(idx('gate')))
    expect(waiting.packet).not.toBeNull()
    expect(waiting.gateDeg).toBe(0)
    expect(machineAt(mid(idx('run'))).packet).toBeNull()
  })

  it('opens the gate only while executing', () => {
    expect(machineAt(STAGE_STARTS[idx('exec')] + STAGES[idx('exec')].ms * 0.5).gateDeg).toBe(80)
    for (const id of ['tool', 'audit']) expect(machineAt(mid(idx(id))).gateDeg).toBe(0)
  })

  it('appends one ledger block per audit', () => {
    const before = machineAt(mid(idx('infer'))).ledger
    expect(machineAt(mid(idx('reply'))).ledger).toBe(before + 1)
  })

  it('clamps negative time', () => {
    expect(machineAt(-500)).toEqual(machineAt(0))
  })

  it('every lit station exists', () => {
    const ids = new Set(STATIONS.map((s) => s.id))
    for (let i = 0; i < STAGES.length; i++) for (const id of machineAt(mid(i)).lit) expect(ids.has(id)).toBe(true)
  })
})

describe('stepTo', () => {
  it('steps forward across the run boundary and never before t=0', () => {
    expect(machineAt(stepTo(mid(STAGES.length - 1), 1))).toMatchObject({ runNo: 1, stageIndex: 0 })
    expect(stepTo(mid(0), -1)).toBe(0)
    expect(machineAt(stepTo(mid(2), -1)).stageIndex).toBe(1)
  })
})

describe('home copy', () => {
  // Stage / station / source keys are built from ids at runtime, so the
  // compiler can't see them — check every id has its words.
  it('has words for every stage, station and source', () => {
    const keys = new Set(Object.keys(homeMessages.en))
    const need = [
      ...STAGES.map((s) => `stage.${s.id}`),
      ...STATIONS.flatMap((s) => ['name', 'sub', 'detail'].map((f) => `st.${s.id}.${f}`)),
      ...SOURCES.flatMap((s) => [`source.${s}`, `source.${s}.back`]),
    ]
    expect(need.filter((k) => !keys.has(k))).toEqual([])
  })
})
