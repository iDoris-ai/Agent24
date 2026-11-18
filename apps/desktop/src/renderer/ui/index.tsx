import type { ButtonHTMLAttributes, InputHTMLAttributes, ReactNode, SelectHTMLAttributes, TextareaHTMLAttributes } from 'react'
import './ui.css'

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & { variant?: 'primary' | 'secondary' | 'ghost' | 'danger'; size?: 'sm' | 'md' }
export function Button({ variant = 'secondary', size = 'md', className = '', ...props }: ButtonProps) {
  return <button {...props} className={`ui-button ui-button--${variant} ui-button--${size} ${className}`.trim()} />
}

type IconButtonProps = ButtonProps & { 'aria-label': string }
export function IconButton(props: IconButtonProps) { return <Button {...props} className={`ui-icon-button ${props.className ?? ''}`} /> }

type FieldProps = { label: string; hint?: string; error?: string; id?: string }
export function TextField({ label, hint, error, id, ...props }: FieldProps & InputHTMLAttributes<HTMLInputElement>) {
  const fieldId = id ?? `field-${label.toLowerCase().replace(/[^a-z0-9]+/g, '-')}`
  const helpId = `${fieldId}-help`
  return <div className="ui-field"><label htmlFor={fieldId}>{label}</label><input {...props} id={fieldId} aria-invalid={error ? true : props['aria-invalid']} aria-describedby={hint || error ? helpId : props['aria-describedby']} />{(hint || error) && <small id={helpId} className={error ? 'ui-field-error' : 'ui-field-hint'}>{error ?? hint}</small>}</div>
}
export function TextArea({ label, hint, error, id, ...props }: FieldProps & TextareaHTMLAttributes<HTMLTextAreaElement>) {
  const fieldId = id ?? `field-${label.toLowerCase().replace(/[^a-z0-9]+/g, '-')}`
  const helpId = `${fieldId}-help`
  return <div className="ui-field"><label htmlFor={fieldId}>{label}</label><textarea {...props} id={fieldId} aria-invalid={error ? true : props['aria-invalid']} aria-describedby={hint || error ? helpId : props['aria-describedby']} />{(hint || error) && <small id={helpId} className={error ? 'ui-field-error' : 'ui-field-hint'}>{error ?? hint}</small>}</div>
}
export function Select({ label, hint, error, id, children, ...props }: FieldProps & SelectHTMLAttributes<HTMLSelectElement>) {
  const fieldId = id ?? `field-${label.toLowerCase().replace(/[^a-z0-9]+/g, '-')}`
  const helpId = `${fieldId}-help`
  return <div className="ui-field"><label htmlFor={fieldId}>{label}</label><select {...props} id={fieldId} aria-invalid={error ? true : props['aria-invalid']} aria-describedby={hint || error ? helpId : props['aria-describedby']}>{children}</select>{(hint || error) && <small id={helpId} className={error ? 'ui-field-error' : 'ui-field-hint'}>{error ?? hint}</small>}</div>
}

export function Card({ title, actions, children, className = '' }: { title?: ReactNode; actions?: ReactNode; children: ReactNode; className?: string }) {
  return <section className={`ui-card ${className}`.trim()}>{(title || actions) && <header className="ui-card-header">{title && <h2>{title}</h2>}{actions && <div className="ui-card-actions">{actions}</div>}</header>}{children}</section>
}
export function SegmentedTabs({ tabs, value, onChange, label }: { tabs: Array<{ value: string; label: string }>; value: string; onChange: (value: string) => void; label: string }) {
  return <div className="ui-segmented" role="tablist" aria-label={label}>{tabs.map((tab) => <button key={tab.value} type="button" role="tab" aria-selected={value === tab.value} onClick={() => onChange(tab.value)}>{tab.label}</button>)}</div>
}
export function Badge({ tone = 'neutral', children }: { tone?: 'neutral' | 'accent' | 'success' | 'warning' | 'danger'; children: ReactNode }) { return <span className={`ui-badge ui-badge--${tone}`}>{children}</span> }
export function EmptyState({ description, action }: { description: string; action: ReactNode }) { return <div className="ui-empty"><p>{description}</p>{action}</div> }
export function ErrorNotice({ message, actionLabel, detailsLabel = 'Details', onAction, details, className = '' }: { message: string; actionLabel: string; detailsLabel?: string; onAction: () => void; details?: ReactNode; className?: string }) {
  return <div className={`ui-error-notice ${className}`.trim()} role="alert"><p>{message}</p><Button variant="secondary" size="sm" onClick={onAction}>{actionLabel}</Button>{details != null && <details><summary>{detailsLabel}</summary><pre>{details}</pre></details>}</div>
}
export function PageHeader({ title, subtitle, actions }: { title: string; subtitle?: string; actions?: ReactNode }) {
  return <header className="ui-page-header"><div><h1>{title}</h1>{subtitle && <p>{subtitle}</p>}</div>{actions && <div className="ui-page-header-actions">{actions}</div>}</header>
}
