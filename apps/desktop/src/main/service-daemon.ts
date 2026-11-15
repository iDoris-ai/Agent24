// The service daemon this app is enrolled with, by value: its pid and its
// generation. Both are set at enrolment and left alone by a renewal
// (backend-manager.ts enrolExternal / renewEnrolment), so a renewal does not
// change it; a restarted daemon, or a switch to our own child, does.
export interface ServiceDaemonSource {
  presenceTarget(): { socketPath: string; generation: string } | null
  externalDaemonPid(): number | null
}

export interface ServiceDaemon { pid: number; generation: string }

/** presenceTarget() first: a reused legacy daemon also has an external pid. */
export function serviceDaemonOf(source: ServiceDaemonSource): ServiceDaemon | null {
  const target = source.presenceTarget()
  if (!target) return null
  const pid = source.externalDaemonPid()
  return pid === null ? null : { pid, generation: target.generation }
}
