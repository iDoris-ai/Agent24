// Whether quitting stops the oMLX this app started (FU-114). With the resident
// service installed, quitting the app leaves the service daemon running (M5 B4),
// and that daemon uses the same oMLX (127.0.0.1:8088 by default): it keeps
// running too. 「完全退出」 removes the LaunchAgent before it quits
// (resident-service.ts stopService), so oMLX is stopped then, as before.
export interface OmlxQuitInput {
  platform: NodeJS.Platform
  /** The LaunchAgent's plist is there, read when the app quits. */
  serviceInstalled: boolean
}

export function stopsOwnedOmlxOnQuit(input: OmlxQuitInput): boolean {
  return !(input.platform === 'darwin' && input.serviceInstalled)
}
