// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest'
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react'
import {
  CapabilitiesContext,
  NavPrefsContext,
  type CapabilityDef,
  type NavPrefs,
} from '../capabilities/types'
import { setLocale } from '../i18n'
import CapabilitiesPage from './Capabilities'
import { capabilitiesMessages } from './Capabilities.i18n'
import { CAPABILITIES } from '../capabilities/registry'
import { registerDynamicMessages } from '../capabilities/capabilities.i18n'

// Shuffled on purpose: group order must follow CAPABILITY_GROUPS, and order
// inside a group must follow the defs array, not a sort by name.
const translateKeys = registerDynamicMessages('translate', 'Translate', 'Translate text')
const DEFS: CapabilityDef[] = ['schedules', 'design', 'assistant', 'translate', 'knowledgeBase', 'sin90', 'documents', 'approvals'].map(id => {
  if (id === 'translate') return { id, group: 'tools', ...translateKeys, builtin: false, page: 'translate' }
  const def = CAPABILITIES.find(def => def.id === id)
  if (!def) throw new Error(`Missing capability ${id}`)
  return def
})


const GROUP_TITLES = ['Office', 'Knowledge', 'Creative', 'Domain OS', 'Tools', 'Services']
const GROUP_BLURBS = [
  'Built into Agent24',
  'Separate services: on this Mac or your team server',
  'Large, downloaded on demand',
  'Loadable modules',
  'Small actions Assistant and Documents can use',
  'Background parts of Agent24',
]

const ROOMY: NavPrefs = { pinned: ['documents'], slotCount: 3, showOverview: true }

function renderPage(opts: {
  defs?: readonly CapabilityDef[]
  prefs?: NavPrefs
  approvalsWaiting?: number
  statusFor?: (id: string) => string | undefined
  onOpenSettings?: () => void
} = {}) {
  cleanup()
  const open = vi.fn()
  const setPrefs = vi.fn(() => Promise.resolve([] as string[]))
  const onOpenSettings = opts.onOpenSettings ?? vi.fn()
  const prefs = opts.prefs ?? ROOMY
  render(
    <CapabilitiesContext.Provider value={{ defs: opts.defs ?? DEFS, open, approvalsWaiting: opts.approvalsWaiting ?? 0 }}>
      <NavPrefsContext.Provider value={{ prefs, setPrefs }}>
        <CapabilitiesPage statusFor={opts.statusFor} onOpenSettings={onOpenSettings} />
      </NavPrefsContext.Provider>
    </CapabilitiesContext.Provider>,
  )
  return { open, setPrefs, onOpenSettings }
}

function row(name: string) {
  return screen.getByRole('listitem', { name })
}

function pinButton(name: string, label: string) {
  return within(row(name)).getByRole('button', { name: `${label} ${name}` })
}

describe('CapabilitiesPage', () => {
  it('groups defs in CAPABILITY_GROUPS order, with a title and blurb, skipping empty groups', () => {
    renderPage({
      defs: DEFS.filter((d) => d.group !== 'tools'),
    })
    expect(screen.getAllByRole('heading', { level: 2 }).map((h) => h.textContent)).toEqual(
      GROUP_TITLES.filter((title) => title !== 'Tools'),
    )
    for (const blurb of GROUP_BLURBS.filter((text) => text !== GROUP_BLURBS[4])) {
      expect(screen.getByText(blurb)).toBeInTheDocument()
    }
    expect(screen.queryByRole('heading', { level: 2, name: 'Tools' })).not.toBeInTheDocument()
    expect(screen.queryByText(GROUP_BLURBS[4])).not.toBeInTheDocument()
    const office = screen.getByRole('region', { name: 'Office' })
    expect(within(office).getAllByRole('listitem').map((li) => li.getAttribute('aria-label'))).toEqual([
      'Assistant',
      'Documents',
    ])
    expect(within(row('Assistant')).getByText('Talk with your built-in AI assistant.')).toBeInTheDocument()
  })

  it('opens a capability from its name and from the Open button', () => {
    const { open } = renderPage()
    fireEvent.click(within(row('Design')).getByRole('button', { name: /^Design$/ }))
    fireEvent.click(within(row('Documents')).getByRole('button', { name: /^Open$/ }))
    expect(open).toHaveBeenNthCalledWith(1, 'design', 'capabilities')
    expect(open).toHaveBeenNthCalledWith(2, 'documents', 'capabilities')
  })

  it('pins, unpins, and disables the control when the row is fixed or the slots are full', () => {
    const roomy = renderPage()
    fireEvent.click(pinButton('Design', 'Pin'))
    expect(roomy.setPrefs).toHaveBeenCalledWith({
      pinned: ['documents', 'design'],
      slotCount: 3,
      showOverview: true,
    })
    expect(pinButton('Design', 'Pin')).toHaveAttribute('aria-pressed', 'false')

    const pinned = renderPage()
    const documentsPin = pinButton('Documents', 'Pinned')
    expect(documentsPin).toBeEnabled()
    expect(documentsPin).toHaveAttribute('aria-pressed', 'true')
    fireEvent.click(documentsPin)
    expect(pinned.setPrefs).toHaveBeenCalledWith({ pinned: [], slotCount: 3, showOverview: true })

    const full = renderPage({ prefs: { pinned: ['documents'], slotCount: 1, showOverview: true } })
    const blocked = pinButton('Design', 'Slots full')
    expect(blocked).toBeDisabled()
    expect(blocked).toHaveAttribute('aria-pressed', 'false')
    fireEvent.click(blocked)
    expect(full.setPrefs).not.toHaveBeenCalled()
    expect(pinButton('Documents', 'Pinned')).toBeEnabled()

    const fixed = renderPage()
    const always = pinButton('Assistant', 'Always shown')
    expect(always).toBeDisabled()
    expect(always).toHaveAttribute('aria-pressed', 'false')
    fireEvent.click(always)
    expect(fixed.setPrefs).not.toHaveBeenCalled()
  })

  it('toggles Show Overview without touching the pinned list', () => {
    const shown = renderPage()
    fireEvent.click(screen.getByRole('checkbox', { name: 'Show Overview in sidebar' }))
    expect(shown.setPrefs).toHaveBeenCalledWith({ pinned: ['documents'], slotCount: 3, showOverview: false })

    const hidden = renderPage({ prefs: { pinned: ['documents', 'design'], slotCount: 4, showOverview: false } })
    const box = screen.getByRole('checkbox', { name: 'Show Overview in sidebar' })
    expect(box).not.toBeChecked()
    fireEvent.click(box)
    expect(hidden.setPrefs).toHaveBeenCalledWith({
      pinned: ['documents', 'design'],
      slotCount: 4,
      showOverview: true,
    })
  })

  it('shows the slots-full note and opens Settings from its link', () => {
    const { open, onOpenSettings } = renderPage({
      prefs: { pinned: ['documents', 'design', 'kb'], slotCount: 3, showOverview: true },
    })
    expect(screen.getByText(/All sidebar slots are in use/)).toHaveTextContent(
      'All sidebar slots are in use. Unpin one to pin another, or add slots in Settings › General.',
    )
    fireEvent.click(screen.getByRole('link', { name: 'Settings › General' }))
    expect(onOpenSettings).toHaveBeenCalledTimes(1)
    expect(open).not.toHaveBeenCalled()

    renderPage()
    expect(screen.queryByText(/All sidebar slots are in use/)).not.toBeInTheDocument()
  })

  it('shows the approvals count only on the approvals row, and only when it is positive', () => {
    renderPage({ approvalsWaiting: 4 })
    expect(within(row('Approvals')).getByRole('status', { name: '4 waiting' })).toHaveTextContent('4')
    expect(within(row('Documents')).queryByRole('status')).not.toBeInTheDocument()

    renderPage({ approvalsWaiting: 0 })
    expect(screen.queryByRole('status', { name: /waiting/ })).not.toBeInTheDocument()
  })

  it('labels builtin rows Built-in, other rows Open, and lets statusFor replace either', () => {
    renderPage({ statusFor: (id) => (id === 'design' ? 'Downloaded' : undefined) })
    expect(within(row('Documents')).getByText('Built-in', { selector: '.cap-status' })).toBeInTheDocument()
    expect(within(row('Translate')).getByText('Open', { selector: '.cap-status' })).toBeInTheDocument()
    expect(within(row('Design')).getByText('Downloaded', { selector: '.cap-status' })).toBeInTheDocument()
  })

  it('uses the same keys in en, zh and th, and relabels when the locale changes', () => {
    const en = Object.keys(capabilitiesMessages.en).sort()
    expect(en.length).toBeGreaterThan(0)
    expect(Object.keys(capabilitiesMessages.zh).sort()).toEqual(en)
    expect(Object.keys(capabilitiesMessages.th).sort()).toEqual(en)
    for (const key of en) {
      expect(capabilitiesMessages.en[key as keyof typeof capabilitiesMessages.en].trim()).not.toBe('')
      expect(capabilitiesMessages.zh[key as keyof typeof capabilitiesMessages.zh].trim()).not.toBe('')
      expect(capabilitiesMessages.th[key as keyof typeof capabilitiesMessages.th].trim()).not.toBe('')
    }

    renderPage()
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Capabilities')
    act(() => setLocale('zh'))
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('能力中心')
    expect(screen.getByText('内置在 Agent24 里')).toBeInTheDocument()
    expect(within(row('文档')).getByText('导入、阅读和处理文档。')).toBeInTheDocument()
    expect(screen.getByRole('checkbox', { name: '在侧边栏显示总览' })).toBeInTheDocument()
    act(() => setLocale('th'))
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('ความสามารถ')
    expect(screen.getByText('มากับ Agent24')).toBeInTheDocument()
    expect(within(row('เอกสาร')).getByText('นำเข้า อ่าน และทำงานกับเอกสารของคุณ')).toBeInTheDocument()
    act(() => setLocale('en'))
  })
})
