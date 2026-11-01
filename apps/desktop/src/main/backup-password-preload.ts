/// <reference lib="dom" />
// Preload for the backup-password window ONLY. It runs sandboxed, so it must
// stay self-contained: the only thing it may import is 'electron'. The page it
// serves has no script of its own (CSP `default-src 'none'`); all behaviour is
// wired here, in the isolated preload world, and nothing is exposed to the page.
//
// The channel names are repeated from backup-password-window.ts (a sandboxed
// preload cannot require sibling modules); a test pins them together.
import { ipcRenderer } from 'electron'

const SUBMIT = 'backup-password:submit'
const CANCEL = 'backup-password:cancel'
const ERROR = 'backup-password:error'
const BUSY = 'backup-password:busy'

window.addEventListener('DOMContentLoaded', () => {
  const form = document.getElementById('f') as HTMLFormElement | null
  const first = document.getElementById('p1') as HTMLInputElement | null
  // Export mode has a second (repeat) field; restore mode has just one.
  const second = document.getElementById('p2') as HTMLInputElement | null
  const cancel = document.getElementById('cancel')
  const go = document.getElementById('go') as HTMLButtonElement | null
  const error = document.getElementById('err')
  if (!form || !first || !cancel || !error) return
  const setBusy = (busy: boolean) => {
    first.disabled = busy
    if (second) second.disabled = busy
    if (go) go.disabled = busy
  }

  form.addEventListener('submit', (event) => {
    event.preventDefault()
    ipcRenderer.send(SUBMIT, first.value, second ? second.value : first.value)
    first.value = ''
    if (second) second.value = ''
  })
  cancel.addEventListener('click', () => ipcRenderer.send(CANCEL))
  window.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') ipcRenderer.send(CANCEL)
  })
  ipcRenderer.on(ERROR, (_event, message: unknown) => {
    setBusy(false)
    error.textContent = typeof message === 'string' ? message : ''
    first.focus()
  })
  ipcRenderer.on(BUSY, (_event, message: unknown) => {
    setBusy(true)
    error.textContent = typeof message === 'string' ? message : ''
  })
})
