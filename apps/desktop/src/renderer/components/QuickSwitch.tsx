import { useContext, useEffect, useId, useRef, useState } from 'react'
import { CapabilitiesContext, NavPrefsContext } from '../capabilities/types'
import { capabilitiesMessages } from '../capabilities/capabilities.i18n'
import { useT } from '../i18n'
import { quickSwitchMessages } from './QuickSwitch.i18n'
import './QuickSwitch.css'

export function QuickSwitch(): JSX.Element | null {
  const { defs, open } = useContext(CapabilitiesContext)
  const { prefs } = useContext(NavPrefsContext)
  const t = useT(quickSwitchMessages)
  const name = useT(capabilitiesMessages)
  const [visible, setVisible] = useState(false)
  const [query, setQuery] = useState('')
  const [index, setIndex] = useState(0)
  const input = useRef<HTMLInputElement>(null)
  const dialog = useRef<HTMLDivElement>(null)
  const previousFocus = useRef<Element | null>(null)
  const id = useId()
  const entries = [
    ...defs.map(def => ({
      id: def.id, label: name(def.nameKey),
      detail: t(prefs.pinned.includes(def.id) ? 'pinned' : 'unpinned'),
    })),
    { id: 'settings/activity', label: t('activity'), detail: t('action') },
    { id: 'settings/general', label: t('general'), detail: t('action') },
    { id: 'capabilities', label: t('capabilities'), detail: t('action') },
  ]
  const needle = query.trim().toLocaleLowerCase()
  const matches = entries.map(entry => {
    const terms = [entry.label, entry.id].map(term => term.toLocaleLowerCase())
    const rank = terms.some(term => term.startsWith(needle)) ? 0 : 1
    return { ...entry, rank, matches: terms.some(term => term.includes(needle)) }
  }).filter(entry => entry.matches).sort((a, b) => a.rank - b.rank)
  const activeIndex = Math.min(index, matches.length - 1)
  const active = matches[activeIndex]
  const activeId = active ? `${id}-option-${activeIndex}` : undefined

  useEffect(() => {
    const onShortcut = (event: KeyboardEvent) => {
      if (event.key.toLowerCase() !== 'k' || !(event.metaKey || event.ctrlKey) || event.altKey || event.isComposing) return
      event.preventDefault()
      if (event.repeat) return
      setQuery('')
      setIndex(0)
      setVisible(current => !current)
    }
    window.addEventListener('keydown', onShortcut, true)
    return () => window.removeEventListener('keydown', onShortcut, true)
  }, [])

  useEffect(() => {
    if (!visible || !dialog.current) return
    previousFocus.current = document.activeElement
    const background: { element: HTMLElement; inert: boolean }[] = []
    // Inert every sibling branch, preserving theme inheritance inside App.
    let branch: HTMLElement = dialog.current
    while (branch.parentElement) {
      for (const sibling of branch.parentElement.children) {
        if (sibling !== branch && sibling instanceof HTMLElement) {
          background.push({ element: sibling, inert: sibling.inert })
          sibling.inert = true
        }
      }
      branch = branch.parentElement
      if (branch === document.body) break
    }
    const containFocus = (event: FocusEvent) => {
      if (event.target instanceof Node && !dialog.current?.contains(event.target)) input.current?.focus()
    }
    document.addEventListener('focusin', containFocus)
    input.current?.focus()
    return () => {
      document.removeEventListener('focusin', containFocus)
      background.forEach(({ element, inert }) => { element.inert = inert })
      if (previousFocus.current instanceof HTMLElement && previousFocus.current.isConnected) previousFocus.current.focus()
    }
  }, [visible])

  useEffect(() => {
    if (visible && activeId) document.getElementById(activeId)?.scrollIntoView({ block: 'nearest' })
  }, [visible, activeId, query])

  function activate(target: string) {
    setVisible(false)
    open(target, 'switcher')
  }

  if (!visible) return null
  return (
    <div className="quick-switch-backdrop" ref={dialog} onClick={event => {
      if (event.target === event.currentTarget) setVisible(false)
    }}>
      <div className="quick-switch" role="dialog" aria-modal="true" aria-labelledby={`${id}-title`}
        onKeyDown={event => {
          if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); setVisible(false) }
          if (event.key === 'Tab') { event.preventDefault(); input.current?.focus() }
        }}>
        <header><h2 id={`${id}-title`}>{t('title')}</h2><kbd>Esc</kbd></header>
        <input ref={input} role="combobox" aria-label={t('search')} placeholder={t('search')}
          aria-expanded="true" aria-autocomplete="list" aria-controls={`${id}-results`}
          aria-activedescendant={activeId} value={query}
          onChange={event => { setQuery(event.target.value); setIndex(0) }}
          onKeyDown={event => {
            if (event.nativeEvent.isComposing) return
            if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
              event.preventDefault()
              if (matches.length) setIndex((activeIndex + (event.key === 'ArrowDown' ? 1 : -1) + matches.length) % matches.length)
            }
            if (event.key === 'Enter') { event.preventDefault(); if (active) activate(active.id) }
          }} />
        <div role="listbox" id={`${id}-results`} aria-label={t('title')} className="quick-switch-results">
          {matches.map((entry, itemIndex) => (
            <div role="option" id={`${id}-option-${itemIndex}`} key={`${itemIndex}-${entry.id}`}
              aria-selected={itemIndex === activeIndex} className="quick-switch-option"
              onMouseDown={event => event.preventDefault()} onClick={() => activate(entry.id)}>
              <span>{entry.label}</span>{' '}<span className="quick-switch-detail">{entry.detail}</span>
            </div>
          ))}
        </div>
        <div role="status" className="quick-switch-empty">{matches.length === 0 ? t('empty') : ''}</div>
      </div>
    </div>
  )
}
