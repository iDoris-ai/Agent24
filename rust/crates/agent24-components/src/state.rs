//! The component lifecycle state machine (ADR-038 D5): the one state set every component
//! presents, the transition table, the per-form subsets, and the mapping from the states
//! that existed before it. Pure data; nothing here talks to a process.

use serde::{Deserialize, Serialize};

/// `absent | installing | installed | starting | ready | degraded | stopping | stopped |
/// incompatible | failed`. `incompatible` and `failed` are not terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentState {
    Absent,
    Installing,
    Installed,
    Starting,
    Ready,
    Degraded,
    Stopping,
    Stopped,
    Incompatible,
    Failed,
}

use ComponentState::{
    Absent, Degraded, Failed, Incompatible, Installed, Installing, Ready, Starting, Stopped,
    Stopping,
};

/// How a component runs; each form uses a subset of the states (D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Form {
    /// iDoris serve, Hyphae daemon, domain OS packages: the whole set.
    Resident,
    /// Open Design: whole set; installation is reported by the desktop.
    Sidecar,
    /// Per-call CLI: no running states.
    Cli,
    /// AgentEar: Agent24 does not manage installation.
    Attach,
    /// Knowledge: not downloaded, so no install states and no `stopping`.
    Remote,
    /// MCP server: `installed / starting / ready / failed / stopped`.
    Mcp,
}

/// D5's table, verbatim: `(from, to)`.
const TABLE: &[(ComponentState, ComponentState)] = &[
    (Absent, Installing),
    (Installing, Installed),
    (Installing, Failed),
    (Installing, Absent),
    (Installed, Starting),
    (Installed, Absent),
    (Installed, Installing),
    (Starting, Ready),
    (Starting, Incompatible),
    (Starting, Failed),
    (Ready, Degraded),
    (Degraded, Ready),
    (Ready, Stopping),
    (Degraded, Stopping),
    (Ready, Failed),
    (Degraded, Failed),
    (Stopping, Stopped),
    (Stopped, Starting),
    (Failed, Starting),
    (Failed, Installing),
    (Incompatible, Installing),
];

impl Form {
    /// Whether this form ever shows `state`.
    #[must_use]
    pub fn allows(self, state: ComponentState) -> bool {
        match self {
            Self::Resident | Self::Sidecar => true,
            Self::Cli => matches!(
                state,
                Absent | Installing | Installed | Incompatible | Failed
            ),
            Self::Attach => !matches!(state, Absent | Installing | Installed),
            Self::Remote => !matches!(state, Absent | Installing | Installed | Stopping),
            Self::Mcp => matches!(state, Installed | Starting | Ready | Failed | Stopped),
        }
    }

    /// Edges D5 states in prose for a subset rather than in the table: a form with no
    /// `stopping` goes straight to `stopped` (a dropped connection, an unconfigured
    /// service, a closed server), and a per-call CLI, which never runs `starting`, learns of a
    /// contract mismatch from the installed binary.
    fn extra(self, from: ComponentState, to: ComponentState) -> bool {
        match (self, to) {
            (Self::Attach | Self::Remote | Self::Mcp, Stopped) => {
                matches!(from, Starting | Ready | Degraded | Failed)
            }
            (Self::Cli, Incompatible) => from == Installed,
            _ => false,
        }
    }

    /// Whether `from -> to` is allowed for this form. A state to itself is not a
    /// transition.
    #[must_use]
    pub fn can_transition(self, from: ComponentState, to: ComponentState) -> bool {
        self.allows(from)
            && self.allows(to)
            && (TABLE.contains(&(from, to)) || self.extra(from, to))
    }
}

/// `SidecarState` (`sidecar-contract.ts`): the same names, and `healthy` is `ready`.
#[must_use]
pub fn from_sidecar(state: &str) -> Option<ComponentState> {
    Some(match state {
        "stopped" => Stopped,
        "starting" => Starting,
        "ready" | "healthy" => Ready,
        "degraded" => Degraded,
        "stopping" => Stopping,
        "failed" => Failed,
        _ => return None,
    })
}

/// `OpenDesignComponentState` (`open-design-component.ts`), with the `reason` D5 attaches
/// to `unavailable`. `downloading` and `verifying` are not in D5's table and map to nothing
/// here; the desktop reports the D5 state itself.
#[must_use]
pub fn from_open_design(state: &str) -> Option<(ComponentState, Option<&'static str>)> {
    Some(match state {
        "not-installed" => (Absent, None),
        "paused" => (Installing, None),
        "installed" => (Installed, None),
        "failed" => (Failed, None),
        "unavailable" => (Absent, Some("unavailable")),
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
