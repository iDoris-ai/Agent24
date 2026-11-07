import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore, type DragEvent } from 'react'
import { useT } from '../../i18n'
import { chatMessages } from '../Chat.i18n'
import {
  clearConversations,
  createGroup,
  deleteConversation,
  deleteGroup,
  getHistoryState,
  moveConversation,
  openConversation,
  refreshGroups,
  refreshHistory,
  renameConversation,
  renameGroup,
  setCollapsed,
  subscribeHistory,
} from './history'
import { graphemes, oneLine, renameTitle } from './title'
import { DRAG_TYPE, dropChange, groupOf, targetKey, type DropTarget } from './drop'
import { HistoryMenu, type MenuEntry } from './HistoryMenu'
import type { ChatHistorySummary } from '../../../shared/chat-history'

/** UI-04 T3: with more conversations than this, the list offers a search. */
export const SEARCH_FROM = 8

/** Local time as `MM-DD HH:mm`, short enough for the sidebar. */
export function shortTime(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  const two = (n: number) => String(n).padStart(2, '0')
  return `${two(d.getMonth() + 1)}-${two(d.getDate())} ${two(d.getHours())}:${two(d.getMinutes())}`
}

/** Text for matching: full- and half-width forms alike, case ignored (the
 *  final sigma folded too, which lowercasing alone leaves apart). */
const fold = (text: string): string => text.normalize('NFKC').toLowerCase().replace(/ς/g, 'σ')
const haystackOf = (item: ChatHistorySummary): string => fold(`${item.title}\n${item.first_prompt}`)

const clipped = (text: string, chars: number): string => {
  const g = graphemes(text)
  return g.length > chars ? `${g.slice(0, chars).join('')}…` : text
}

/** A one-line box for a group's name: Enter or leaving it keeps what was
 *  typed, Escape drops it; an input method's composition keys are its own. */
function NameBox({ label, initial, onDone }: { label: string; initial: string; onDone: (name: string | null) => void }) {
  const [value, setValue] = useState(initial)
  const done = useRef(false)
  const finish = (name: string | null) => {
    if (done.current) return
    done.current = true
    onDone(name)
  }
  return <input className="chat-history-rename" aria-label={label} value={value} autoFocus
    onFocus={e => e.currentTarget.select()} onChange={e => setValue(e.target.value)}
    onKeyDown={e => {
      if (e.nativeEvent.isComposing || e.keyCode === 229) return
      if (e.key === 'Enter') { e.preventDefault(); finish(value) }
      else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); finish(null) }
    }}
    onBlur={() => finish(value)} />
}

type Menu = { kind: 'row' | 'group'; id: string; at: { x: number; y: number }; from: HTMLElement | null }
type Opener = { currentTarget: HTMLElement; clientX?: number; clientY?: number }

/** UI-03: the main chat's recent conversations, listed in the sidebar under
 *  the assistant. Clicking one shows the chat page with that conversation.
 *  UI-04: each can be renamed (a double click, F2 or its menu). UI-06: the
 *  user's groups, folded or not, above the ungrouped conversations; a
 *  conversation moves by dragging it onto a group's heading (or onto
 *  "Recent", out of any) or through its ⋯ menu, also opened by a right
 *  click; the whole list folds. Deleting asks first, as clearing does. */
export function ChatHistoryList({ showChat }: { showChat: () => void }) {
  const t = useT(chatMessages)
  const { items, activeId, busy, groups: groupState } = useSyncExternalStore(subscribeHistory, getHistoryState)
  const groups = groupState.groups
  useEffect(() => { void refreshHistory(); void refreshGroups() }, [])
  const [query, setQuery] = useState('')
  const searchRef = useRef<HTMLInputElement>(null)
  // Archived conversations are not listed with the others (G3 lists them apart).
  const live = useMemo(() => items.filter(item => item.archived_at === null), [items])
  // What each row shows, worked out once per list, not on every keystroke
  // of a rename or a search. (`t` is new on every render; its text is not.)
  const untitled = t('newChat')
  const rows = useMemo(() => {
    // Titles shared by more than one conversation get their time beside them.
    const titleCount = new Map<string, number>()
    for (const item of live) {
      const name = item.title || untitled
      titleCount.set(name, (titleCount.get(name) ?? 0) + 1)
    }
    return live.map(item => {
      const name = item.title || untitled
      const when = shortTime(item.updated_at)
      const twin = (titleCount.get(name) ?? 0) > 1 && when !== ''
      return {
        item, name, when, twin,
        group: groupOf(item, groups),
        // Buttons of conversations with the same title say which is which.
        title: twin ? `${name} ${when}` : name,
        tip: [name, item.first_prompt && item.first_prompt !== name ? clipped(item.first_prompt, 80) : '', when]
          .filter(Boolean).join(' · '),
        haystack: haystackOf(item),
      }
    })
  }, [live, groups, untitled])
  type Row = (typeof rows)[number]
  // The search, and so its filter, only while the list is long enough to
  // have one; a query left when the box goes is dropped, so it does not
  // come back with the box (Low U-12).
  const searchable = items.length > SEARCH_FROM
  useEffect(() => { if (!searchable) setQuery('') }, [searchable])
  const needle = fold(oneLine(query))
  const searching = searchable && needle !== ''
  const shown = searching ? rows.filter(row => row.haystack.includes(needle)) : rows

  // The conversation being renamed, the title it had when the box opened,
  // and what has been typed so far. The ref makes Enter followed by the
  // input's blur rename once, not twice.
  const [editing, setEditing] = useState<string | null>(null)
  const [draft, setDraft] = useState('')
  const editingRef = useRef<{ id: string; from: string } | null>(null)
  const listRef = useRef<HTMLDivElement>(null)
  const entry = (id: string) => listRef.current?.querySelector<HTMLButtonElement>(`[data-history-id="${id}"]`)
  const startEdit = (id: string, title: string) => {
    editingRef.current = { id, from: title }
    setEditing(id)
    setDraft(title)
  }
  const endEdit = (save: boolean, refocus: boolean) => {
    const edit = editingRef.current
    if (edit === null) return
    const { id, from } = edit
    editingRef.current = null
    setEditing(null)
    // Only what the user changed: a box left as it opened renames nothing,
    // even if the title changed meanwhile (an earlier rename landing). Compared
    // before the cut to 80 characters, so shortening a longer title counts.
    const title = renameTitle(draft)
    const renamed = save && title && oneLine(draft) !== oneLine(from) ? renameConversation(id, title) : null
    // From the keyboard, focus goes back to the conversation's entry. When
    // the new title takes the row out of the results and focus went with it
    // (it was on the row, or on its buttons after Tab), it moves to the
    // search box.
    if (refocus) requestAnimationFrame(() => entry(id)?.focus())
    void renamed?.then(() => requestAnimationFrame(() => {
      const lost = document.activeElement === null || document.activeElement === document.body
      if (lost && !entry(id)) searchRef.current?.focus()
    }))
  }

  // A group's name being typed: a new group (`group` null), maybe asked for
  // from a conversation's menu to move it there, or a rename.
  const [naming, setNaming] = useState<{ group: string | null; moving: string | null } | null>(null)
  const nameDone = async (name: string | null) => {
    const job = naming
    setNaming(null)
    const text = name === null ? '' : renameTitle(name)
    if (!job || !text) return
    if (job.group !== null) {
      if (text !== groups.find(g => g.id === job.group)?.name) await renameGroup(job.group, text)
      return
    }
    const made = await createGroup(text)
    if (made && job.moving) await moveConversation(job.moving, made)
  }
  const removeGroup = (id: string) => {
    const group = groups.find(g => g.id === id)
    if (!group) return
    // Its conversations stay, back in Recent; only a group holding some asks.
    const count = rows.filter(row => row.group === id).length
    if (count === 0 || window.confirm(t('history.group.deleteConfirm', { name: group.name, count: String(count) }))) {
      void deleteGroup(id)
    }
  }
  const remove = (row: Row) => {
    if (window.confirm(t('history.deleteConfirm', { title: row.title }))) void deleteConversation(row.item.id)
  }

  const [menu, setMenu] = useState<Menu | null>(null)
  const menuRef = useRef<Menu | null>(null)
  menuRef.current = menu
  const closeMenu = useCallback((refocus: boolean) => {
    const from = menuRef.current?.from
    setMenu(null)
    if (refocus) requestAnimationFrame(() => from?.focus())
  }, [])
  /** From its ⋯ button, a menu opens under it; from a right click, where
   *  the pointer is. */
  const openMenu = (kind: Menu['kind'], id: string, e: Opener, pointer = false) => {
    const box = e.currentTarget.getBoundingClientRect()
    const at = pointer && e.clientX !== undefined && e.clientY !== undefined
      ? { x: e.clientX, y: e.clientY }
      : { x: box.left, y: box.bottom }
    setMenu({ kind, id, at, from: e.currentTarget })
  }
  const rowMenu = (row: Row): MenuEntry[] => [
    { kind: 'item', label: t('history.menu.rename'), onSelect: () => startEdit(row.item.id, row.item.title) },
    { kind: 'heading', label: t('history.menu.moveTo') },
    ...groups.filter(g => g.id !== row.group).map((g): MenuEntry => ({
      kind: 'item', label: g.name, onSelect: () => void moveConversation(row.item.id, g.id),
    })),
    { kind: 'item', label: t('history.menu.newGroup'), onSelect: () => setNaming({ group: null, moving: row.item.id }) },
    ...(row.group !== null
      ? [{ kind: 'item', label: t('history.menu.ungroup'), onSelect: () => void moveConversation(row.item.id, null) } as const]
      : []),
    { kind: 'item', label: t('history.menu.delete'), disabled: busy && row.item.id === activeId, onSelect: () => remove(row) },
  ]
  const groupMenu = (id: string): MenuEntry[] => [
    { kind: 'item', label: t('history.group.rename'), onSelect: () => setNaming({ group: id, moving: null }) },
    { kind: 'item', label: t('history.group.delete'), onSelect: () => removeGroup(id) },
  ]

  // Dragging: the conversation being dragged (what a drag carries cannot be
  // read until the drop), and the target it is over.
  const [dragging, setDragging] = useState<string | null>(null)
  const [over, setOver] = useState<string | null>(null)
  const dropProps = (target: DropTarget) => {
    const key = targetKey(target)
    const change = () => {
      const item = dragging === null ? undefined : live.find(i => i.id === dragging)
      return item ? { id: item.id, patch: dropChange(target, item, groups) } : null
    }
    return {
      onDragOver: (e: DragEvent) => {
        // Only a drop that would change something is offered.
        if (!change()?.patch) return
        e.preventDefault()
        if (e.dataTransfer) e.dataTransfer.dropEffect = 'move'
        if (over !== key) setOver(key)
      },
      onDragLeave: (e: DragEvent) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setOver(o => (o === key ? null : o))
      },
      onDrop: (e: DragEvent) => {
        const drop = change()
        setOver(null)
        setDragging(null)
        if (!drop?.patch) return
        e.preventDefault()
        // Moving is undone as easily as it is done: no confirmation.
        if (drop.patch.group_id !== undefined) void moveConversation(drop.id, drop.patch.group_id)
      },
    }
  }

  if (items.length === 0 && groups.length === 0) return null
  const folded = groupState.assistant_collapsed

  const renderRow = (row: Row) => {
    const { item, name, when, twin, title, tip } = row
    const renaming = editing === item.id
    return (
      <li key={item.id}
        className={`chat-history-item${item.id === activeId ? ' active' : ''}${dragging === item.id ? ' dragging' : ''}`}
        draggable={!renaming}
        onDragStart={e => {
          e.dataTransfer?.setData(DRAG_TYPE, item.id)
          if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move'
          setDragging(item.id)
        }}
        onDragEnd={() => { setDragging(null); setOver(null) }}
        onContextMenu={e => {
          if (renaming) return
          e.preventDefault()
          openMenu('row', item.id, e, true)
        }}>
        {renaming
          ? <input className="chat-history-rename" aria-label={t('history.renameInput')} value={draft}
            autoFocus onFocus={e => e.currentTarget.select()}
            onChange={e => setDraft(e.target.value)}
            onKeyDown={e => {
              // Enter and Escape during an input method's composition
              // pick or drop a candidate; they are not ours.
              if (e.nativeEvent.isComposing || e.keyCode === 229) return
              if (e.key === 'Enter') { e.preventDefault(); endEdit(true, true) }
              else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); endEdit(false, true) }
            }}
            onBlur={() => endEdit(true, false)} />
          : <button type="button" className="chat-history-open" title={tip} data-history-id={item.id}
            disabled={busy && item.id !== activeId}
            aria-current={item.id === activeId ? 'true' : undefined}
            onClick={() => { openConversation(item.id); showChat() }}
            onDoubleClick={() => startEdit(item.id, item.title)}
            onKeyDown={e => { if (e.key === 'F2') { e.preventDefault(); startEdit(item.id, item.title) } }}>
            <span className="chat-history-name">{name}</span>
            {twin && <>{' '}<span className="chat-history-when">{when}</span></>}
            {searching && row.group !== null && <>{' '}<span className="chat-history-tag">
              {groups.find(g => g.id === row.group)?.name}</span></>}
          </button>}
        {!renaming && (
          <button type="button" className="chat-history-action" aria-label={t('history.menu.label', { title })}
            aria-haspopup="menu" aria-expanded={menu?.kind === 'row' && menu.id === item.id}
            onClick={e => openMenu('row', item.id, e)}>⋯</button>
        )}
      </li>
    )
  }

  const menuRow = menu?.kind === 'row' ? rows.find(row => row.item.id === menu.id) : undefined
  const menuGroup = menu?.kind === 'group' ? groups.find(g => g.id === menu.id) : undefined
  return (
    <section className="chat-history" aria-label={t('history.label')}>
      <div className={`chat-history-head${over === 'recent' ? ' drop-over' : ''}`} {...dropProps({ kind: 'recent' })}>
        <button type="button" className="chat-history-fold" aria-expanded={!folded}
          aria-label={t(folded ? 'history.fold.show' : 'history.fold.hide')}
          onClick={() => void setCollapsed('assistant', !folded)}>{folded ? '▸' : '▾'}</button>
        <span className="chat-history-head-label">{t('history.recent')}</span>
        {!folded && <>
          <button type="button" className="chat-history-clear" onClick={() => setNaming({ group: null, moving: null })}>
            {t('history.group.new')}</button>
          <button type="button" className="chat-history-clear" disabled={busy} onClick={() => {
            if (window.confirm(t('history.clearConfirm'))) void clearConversations()
          }}>{t('history.clear')}</button>
        </>}
      </div>
      {!folded && <div ref={listRef}>
        {searchable && (
          <input type="search" className="chat-history-search" aria-label={t('history.search')} ref={searchRef}
            placeholder={t('history.search')} value={query} onChange={e => setQuery(e.target.value)}
            onKeyDown={e => {
              // Escape during an input method's composition drops the candidate.
              if (e.nativeEvent.isComposing || e.keyCode === 229) return
              if (e.key === 'Escape' && query !== '') { e.preventDefault(); e.stopPropagation(); setQuery('') }
            }} />
        )}
        {naming?.group === null && (
          <div className="chat-history-group-head">
            <NameBox label={t('history.group.nameInput')} initial="" onDone={name => void nameDone(name)} />
          </div>
        )}
        {searching
          ? <>
            {shown.length === 0 && <p className="chat-history-empty" role="status">{t('history.noMatch')}</p>}
            <ul className="chat-history-list">{shown.map(renderRow)}</ul>
          </>
          : <>
            {groups.map(group => {
              const members = rows.filter(row => row.group === group.id)
              const key = targetKey({ kind: 'group', id: group.id })
              return (
                <div key={group.id} className="chat-history-group" role="group" aria-label={group.name}>
                  <div className={`chat-history-group-head${over === key ? ' drop-over' : ''}`}
                    {...dropProps({ kind: 'group', id: group.id })}
                    onContextMenu={e => {
                      if (naming?.group === group.id) return
                      e.preventDefault()
                      openMenu('group', group.id, e, true)
                    }}>
                    <button type="button" className="chat-history-fold" aria-expanded={!group.collapsed}
                      aria-label={t(group.collapsed ? 'history.group.show' : 'history.group.hide', { name: group.name })}
                      onClick={() => void setCollapsed(group.id, !group.collapsed)}>{group.collapsed ? '▸' : '▾'}</button>
                    {naming?.group === group.id
                      ? <NameBox label={t('history.group.nameInput')} initial={group.name} onDone={name => void nameDone(name)} />
                      : <span className="chat-history-group-name"
                        onDoubleClick={() => setNaming({ group: group.id, moving: null })}>{group.name}</span>}
                    <span className="chat-history-when">{members.length}</span>
                    <button type="button" className="chat-history-action" aria-label={t('history.group.menu', { name: group.name })}
                      aria-haspopup="menu" aria-expanded={menu?.kind === 'group' && menu.id === group.id}
                      onClick={e => openMenu('group', group.id, e)}>⋯</button>
                  </div>
                  {!group.collapsed && <ul className="chat-history-list">{members.map(renderRow)}</ul>}
                </div>
              )
            })}
            <ul className="chat-history-list">{rows.filter(row => row.group === null).map(renderRow)}</ul>
          </>}
      </div>}
      {menu && menuRow && <HistoryMenu label={t('history.menu.label', { title: menuRow.title })}
        entries={rowMenu(menuRow)} at={menu.at} onClose={closeMenu} />}
      {menu && menuGroup && <HistoryMenu label={t('history.group.menu', { name: menuGroup.name })}
        entries={groupMenu(menuGroup.id)} at={menu.at} onClose={closeMenu} />}
    </section>
  )
}
