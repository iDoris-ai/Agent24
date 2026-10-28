// Desktop navigation redesign (2026-10-28, design canvas "Agent24 菜单与能力中心设计稿").
//
// Frozen seam shared by the parallel slices D1 (sidebar + routing + prefs),
// D2 (Capabilities page), D3 (Settings tabs) and D4 (⌘K switcher). Change it
// only in a PR that updates every consumer.
//
// The sidebar keeps four fixed entries (Overview, which the user may hide;
// Assistant; Capabilities; Settings) plus a user-sized group of pinned
// capabilities. Every other feature is reached from the Capabilities page or ⌘K.

import { createContext } from 'react'

/** Groups follow the product architecture, in display order. */
export const CAPABILITY_GROUPS = [
  'office',
  'knowledge',
  'creative',
  'domainOs',
  'tools',
  'services',
] as const
export type CapabilityGroup = (typeof CAPABILITY_GROUPS)[number]

export type CapabilityId = string

export interface CapabilityDef {
  id: CapabilityId
  group: CapabilityGroup
  /** Keys into the capabilities i18n table (D1 owns `capabilities.i18n.ts`). */
  nameKey: string
  descKey: string
  /** Ships inside Agent24; cannot be removed (Assistant, Documents). */
  builtin: boolean
  /** Always in the sidebar's fixed area; never pinnable (Assistant). */
  fixed?: boolean
  /** Page id App routes to when the capability is opened. */
  page: string
}

export interface NavPrefs {
  /** Pinned capability ids, in sidebar order. */
  pinned: CapabilityId[]
  /** How many pinned slots the sidebar offers. */
  slotCount: number
  showOverview: boolean
}

export const SLOT_MIN = 1
export const SLOT_MAX = 6
export const NAV_PREFS_DEFAULT: NavPrefs = {
  pinned: ['documents', 'design', 'create'],
  slotCount: 3,
  showOverview: true,
}

/**
 * Normalise stored prefs: clamp the slot count, drop unknown, fixed and
 * duplicate ids, and unpin whatever no longer fits. `dropped` lists the ids
 * that were unpinned because of the slot limit, so the UI can say so.
 * Storage normalization reserves slots for unknown saved ids until they can
 * be explicitly unpinned; display normalization hides those ids.
 */
export function clampPrefs(
  prefs: NavPrefs,
  defs: readonly CapabilityDef[],
  preserveUnknown = false,
): { prefs: NavPrefs; dropped: CapabilityId[] } {
  const known = new Set(defs.map(d => d.id))
  const pinnable = new Set(defs.filter((d) => !d.fixed).map((d) => d.id))
  const raw = Number.isFinite(prefs.slotCount) ? Math.round(prefs.slotCount) : NAV_PREFS_DEFAULT.slotCount
  const seen = new Set<CapabilityId>()
  const valid = prefs.pinned.filter((id) => (pinnable.has(id) || (preserveUnknown && !known.has(id))) && !seen.has(id) && seen.add(id) !== undefined)
  const hiddenCount = valid.filter(id => !known.has(id)).length
  const slotCount = Math.min(SLOT_MAX, Math.max(SLOT_MIN, raw, hiddenCount))
  let visibleSlots = slotCount - hiddenCount
  const pinned = valid.filter(id => !known.has(id) || visibleSlots-- > 0).slice(0, slotCount)
  return {
    prefs: { pinned, slotCount, showOverview: prefs.showOverview !== false },
    dropped: valid.filter(id => !pinned.includes(id)),
  }
}

/** Where the user came from when a capability page opens (for the breadcrumb). */
export type OpenOrigin = 'sidebar' | 'capabilities' | 'switcher' | 'other'

export interface CapabilitiesApi {
  defs: readonly CapabilityDef[]
  open: (id: CapabilityId, origin: OpenOrigin) => void
  /** Pending approvals count, shown as a badge on Capabilities and Overview. */
  approvalsWaiting: number
}

export interface NavPrefsApi {
  prefs: NavPrefs
  /** Includes saved pins whose manifests are temporarily unavailable. */
  pinnedCount?: number
  /** Updaters run against the latest saved prefs, including hidden ids, after startup reading.
   * Persists through the main process; resolves with ids unpinned by the limit. */
  setPrefs: (next: NavPrefs | ((prev: NavPrefs) => NavPrefs)) => Promise<CapabilityId[]>
}

export const CapabilitiesContext = createContext<CapabilitiesApi>({
  defs: [],
  open: () => {},
  approvalsWaiting: 0,
})

export const NavPrefsContext = createContext<NavPrefsApi>({
  prefs: NAV_PREFS_DEFAULT,
  setPrefs: async () => [],
})
