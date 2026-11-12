// M6 C2: a banner across the top of a page that says one thing and takes the
// user where it is about, the main chat's "The assistant has something to
// say (n)" first. A button: reached and pressed with the keyboard as any.

export interface BannerProps {
  /** What it says, as shown. */
  text: string
  /** Its accessible name and its tooltip (`title`), when it says more
   *  than `text` (an exact count the text caps). */
  label?: string
  /** Where a press takes the user. */
  onActivate: () => void
  /** Pressing does nothing now (e.g. a reply is in flight); still shown. */
  disabled?: boolean
}

export function Banner(props: BannerProps) {
  return <div className="banner"><button type="button" className="banner-action"
    onClick={props.onActivate} disabled={props.disabled}
    aria-label={props.label} title={props.label}>{props.text}</button></div>
}
