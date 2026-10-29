// @vitest-environment jsdom
import { act, fireEvent, render, screen, within } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { CapabilitiesContext, NavPrefsContext, type CapabilityDef } from '../capabilities/types'
import { setLocale } from '../i18n'
import { QuickSwitch } from './QuickSwitch'

const defs: CapabilityDef[] = [
  { id: 'my-documents', group: 'office', nameKey: 'My documents', descKey: '', builtin: false, page: 'custom' },
  { id: 'documents', group: 'office', nameKey: 'documents.name', descKey: '', builtin: true, page: 'documents' },
  { id: 'design', group: 'creative', nameKey: 'design.name', descKey: '', builtin: false, page: 'creative' },
  { id: 'assistant', group: 'office', nameKey: 'assistant.name', descKey: '', builtin: true, fixed: true, page: 'chat' },
]

function setup(capabilities = defs) {
  const open = vi.fn()
  const view = render(
    <CapabilitiesContext.Provider value={{ defs: capabilities, open, approvalsWaiting: 0 }}>
      <NavPrefsContext.Provider value={{ prefs: { pinned: ['documents'], slotCount: 1, showOverview: true }, setPrefs: vi.fn() }}>
        <input aria-label="Previous input" />
        <button>Outside</button>
        <QuickSwitch />
      </NavPrefsContext.Provider>
    </CapabilitiesContext.Provider>,
  )
  const previous = screen.getByRole('textbox', { name: 'Previous input' })
  previous.focus()
  return { ...view, open, previous }
}

function shortcut(target: HTMLElement | Window = window, modifier: 'metaKey' | 'ctrlKey' = 'metaKey') {
  fireEvent.keyDown(target, { key: 'k', [modifier]: true })
}

function search(value: string) {
  fireEvent.change(screen.getByRole('combobox'), { target: { value } })
}

function selected() {
  const input = screen.getByRole('combobox')
  const id = input.getAttribute('aria-activedescendant')
  return id ? document.getElementById(id) : null
}

describe('QuickSwitch', () => {
  it.each(['metaKey', 'ctrlKey'] as const)('opens from a focused input with %s+K, toggles and closes with Escape', modifier => {
    const { previous } = setup()
    shortcut(previous, modifier)
    expect(screen.getByRole('dialog', { name: 'Quick switch' })).toHaveAttribute('aria-modal', 'true')
    expect(screen.getByRole('combobox')).toHaveFocus()
    shortcut(screen.getByRole('combobox'), modifier)
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    shortcut(previous, modifier)
    fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Escape' })
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    fireEvent.keyDown(window, { key: 'k' })
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it('filters localized names and ids, ranking prefixes before substrings with stable ties', () => {
    setup()
    shortcut()
    search('  DOC  ')
    expect(screen.getAllByRole('option').map(item => item.textContent)).toEqual([
      'Documents Capability · pinned', 'My documents Capability · not pinned',
    ])
    search('my-doc')
    expect(screen.getAllByRole('option')).toHaveLength(1)
    expect(screen.getByRole('option')).toHaveTextContent('My documents')
    search('')
    expect(screen.getAllByRole('option').slice(0, 2).map(item => item.textContent)).toEqual([
      'My documents Capability · not pinned', 'Documents Capability · pinned',
    ])
  })

  it('selects with arrows, wraps, and opens the selected capability with switcher origin', () => {
    const { open, previous } = setup()
    shortcut()
    const input = screen.getByRole('combobox')
    expect(selected()).toHaveTextContent('My documents')
    fireEvent.keyDown(input, { key: 'ArrowDown' })
    expect(selected()).toHaveAttribute('aria-selected', 'true')
    expect(selected()).toHaveTextContent('Documents')
    fireEvent.keyDown(input, { key: 'ArrowUp' })
    fireEvent.keyDown(input, { key: 'ArrowUp' })
    expect(selected()).toHaveTextContent('Capabilities')
    fireEvent.keyDown(input, { key: 'ArrowDown' })
    fireEvent.keyDown(input, { key: 'ArrowDown' })
    fireEvent.keyDown(input, { key: 'Enter' })
    expect(open).toHaveBeenCalledExactlyOnceWith('documents', 'switcher')
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    expect(previous).toHaveFocus()
  })

  it('resets selection after filtering and handles empty results and IME Enter safely', () => {
    const { open } = setup()
    shortcut()
    const input = screen.getByRole('combobox')
    fireEvent.keyDown(input, { key: 'ArrowUp' })
    search('design')
    expect(selected()).toHaveTextContent('Design')
    fireEvent.keyDown(input, { key: 'Enter', isComposing: true })
    expect(open).not.toHaveBeenCalled()
    search('no-such-capability')
    expect(screen.getByRole('status')).toHaveTextContent('No results')
    expect(input).not.toHaveAttribute('aria-activedescendant')
    fireEvent.keyDown(input, { key: 'ArrowDown' })
    fireEvent.keyDown(input, { key: 'Enter' })
    expect(open).not.toHaveBeenCalled()
    search('design')
    fireEvent.keyDown(input, { key: 'Enter' })
    expect(open).toHaveBeenCalledExactlyOnceWith('design', 'switcher')
  })

  it('reads pinned state from NavPrefsContext and supports pointer selection', () => {
    const { open } = setup()
    shortcut()
    expect(within(screen.getByRole('option', { name: 'Documents Capability · pinned' })).getByText('Capability · pinned')).toBeVisible()
    expect(screen.getByRole('option', { name: 'Design Capability · not pinned' })).toBeVisible()
    fireEvent.click(screen.getByRole('option', { name: 'Design Capability · not pinned' }))
    expect(open).toHaveBeenCalledExactlyOnceWith('design', 'switcher')
  })

  it.each([
    ['Settings › Activity', 'settings/activity'],
    ['Settings › General', 'settings/general'],
    ['Capabilities', 'capabilities'],
  ])('opens fixed action %s through the context', (label, id) => {
    const { open } = setup([])
    shortcut()
    search(label)
    fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Enter' })
    expect(open).toHaveBeenCalledExactlyOnceWith(id, 'switcher')
  })

  it('traps Tab and programmatic focus, restores focus and removes listeners on unmount', () => {
    const { previous, unmount } = setup()
    const outside = screen.getByRole('button', { name: 'Outside' })
    shortcut()
    const input = screen.getByRole('combobox')
    expect(outside.inert).toBe(true)
    for (const shiftKey of [false, true]) {
      const event = new KeyboardEvent('keydown', { key: 'Tab', shiftKey, bubbles: true, cancelable: true })
      fireEvent(input, event)
      expect(event.defaultPrevented).toBe(true)
      expect(input).toHaveFocus()
    }
    outside.focus()
    expect(input).toHaveFocus()
    fireEvent.keyDown(input, { key: 'Escape' })
    expect(previous).toHaveFocus()
    expect(outside.closest('[inert]')).toBeNull()
    shortcut(previous)
    unmount()
    shortcut()
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it.each([
    ['en', 'Quick switch', 'Search capabilities and actions…', 'Documents', 'Capability · pinned', 'Settings › General', 'No results'],
    ['zh', '快速切换', '搜索能力和操作…', '文档', '能力 · 已钉选', '设置 › 通用', '没有结果'],
    ['th', 'สลับด่วน', 'ค้นหาความสามารถและการทำงาน…', 'เอกสาร', 'ความสามารถ · ปักหมุดแล้ว', 'ตั้งค่า › ทั่วไป', 'ไม่พบผลลัพธ์'],
  ] as const)('localizes copy and capability-name search in %s', (locale, title, placeholder, name, pinned, general, empty) => {
    setup()
    shortcut()
    act(() => setLocale(locale))
    expect(screen.getByRole('dialog', { name: title })).toBeVisible()
    expect(screen.getByPlaceholderText(placeholder)).toHaveFocus()
    expect(screen.getByText(general)).toBeVisible()
    search(name)
    expect(screen.getByRole('option', { name: `${name} ${pinned}` })).toBeVisible()
    search('no-such-capability')
    expect(screen.getByRole('status')).toHaveTextContent(empty)
  })
})
