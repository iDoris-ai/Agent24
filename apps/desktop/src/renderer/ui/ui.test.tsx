// @vitest-environment jsdom
import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { Badge, Button, Card, EmptyState, ErrorNotice, IconButton, PageHeader, SegmentedTabs, Select, TextArea, TextField } from './index'

describe('shared UI components', () => {
  it('renders and activates Button variants', () => {
    const click = vi.fn()
    render(<Button variant="primary" onClick={click}>Save</Button>)
    fireEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(click).toHaveBeenCalledOnce()
  })
  it('requires an accessible name on IconButton', () => {
    render(<IconButton aria-label="Close">×</IconButton>)
    expect(screen.getByRole('button', { name: 'Close' })).toBeTruthy()
  })
  it('associates TextField labels and errors', () => {
    render(<TextField label="Email" error="Enter an email" />)
    expect(screen.getByLabelText('Email').getAttribute('aria-invalid')).toBe('true')
    expect(screen.getByText('Enter an email')).toBeTruthy()
  })
  it('associates TextArea labels', () => { render(<TextArea label="Notes" />); expect(screen.getByLabelText('Notes')).toBeTruthy() })
  it('associates Select labels', () => { render(<Select label="Choice"><option>One</option></Select>); expect(screen.getByLabelText('Choice')).toBeTruthy() })
  it('renders Card heading and actions', () => { render(<Card title="Title" actions={<button>Manage</button>}>Body</Card>); expect(screen.getByRole('heading', { name: 'Title' })).toBeTruthy(); expect(screen.getByRole('button', { name: 'Manage' })).toBeTruthy() })
  it('exposes SegmentedTabs semantics and changes selection', () => {
    const change = vi.fn()
    render(<SegmentedTabs label="View" value="one" onChange={change} tabs={[{ value: 'one', label: 'One' }, { value: 'two', label: 'Two' }]} />)
    expect(screen.getByRole('tab', { name: 'One' }).getAttribute('aria-selected')).toBe('true')
    fireEvent.click(screen.getByRole('tab', { name: 'Two' }))
    expect(change).toHaveBeenCalledWith('two')
  })
  it('renders Badge tone text', () => { render(<Badge tone="success">Ready</Badge>); expect(screen.getByText('Ready')).toBeTruthy() })
  it('renders EmptyState action', () => { render(<EmptyState description="Nothing here" action={<button>Create</button>} />); expect(screen.getByRole('button', { name: 'Create' })).toBeTruthy() })
  it('shows concise ErrorNotice and invokes retry, with details collapsed', () => {
    const retry = vi.fn()
    render(<ErrorNotice message="Try again" actionLabel="Retry" detailsLabel="Details" onAction={retry} details="raw stack" />)
    expect(screen.getByRole('alert').textContent).toContain('Try again')
    expect(screen.getByText('raw stack').closest('details')?.open).toBe(false)
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }))
    expect(retry).toHaveBeenCalledOnce()
    fireEvent.click(screen.getByText('Details'))
    expect(screen.getByText('raw stack')).toBeTruthy()
  })
  it('renders PageHeader title, subtitle, and actions', () => { render(<PageHeader title="Home" subtitle="Welcome" actions={<button>New</button>} />); expect(screen.getByRole('heading', { name: 'Home' })).toBeTruthy(); expect(screen.getByText('Welcome')).toBeTruthy(); expect(screen.getByRole('button', { name: 'New' })).toBeTruthy() })
})
