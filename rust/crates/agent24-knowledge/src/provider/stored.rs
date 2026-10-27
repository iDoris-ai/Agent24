//! How the Provider's records are stored (KB-C1-04 6d): a shape declared here,
//! apart from [`Change`] itself, so a rename in the in-memory type never
//! changes what a stored record means. Serde derives the two-way mapping
//! from the declarations below (`remote`), each of which must match the type
//! it mirrors or the build fails; the stored names are the ones written here.
//! A record reads only as the JSON this build writes it: another version, a
//! kind, a value or a field it does not know, or another shape is refused,
//! and the log reads as damaged (S1.8). Pure: the host stores the text.

use std::collections::BTreeSet;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::evidence::{Kbs, Rule};
use super::profile::Confirmation;
use super::record::{Change, Ids, Of, StuckCause};
use super::replay::Damaged;
use super::state::PlaceholderFailure;

/// The stored shape's version. A record of any other is refused.
pub const VERSION: u32 = 1;

/// The host's values, each stored as it serializes. Each must be exactly
/// the JSON it writes, read back: a string, an integer, or a struct of them;
/// never a map, whose doubled key serde would take, or a float, rounded.
pub trait StoredIds:
    Ids<
        Profile: Serialize + DeserializeOwned,
        Binding: Serialize + DeserializeOwned,
        Run: Serialize + DeserializeOwned,
        Principal: Serialize + DeserializeOwned,
        Identity: Serialize + DeserializeOwned,
        Kb: Serialize + DeserializeOwned,
    >
{
}

impl<I> StoredIds for I where
    I: Ids<
            Profile: Serialize + DeserializeOwned,
            Binding: Serialize + DeserializeOwned,
            Run: Serialize + DeserializeOwned,
            Principal: Serialize + DeserializeOwned,
            Identity: Serialize + DeserializeOwned,
            Kb: Serialize + DeserializeOwned,
        >
{
}

/// One write's changes, as text.
pub fn encode<I: StoredIds>(changes: &[Change<I>]) -> serde_json::Result<String> {
    let changes = changes.iter().cloned().map(Item).collect();
    serde_json::to_string(&Body::<I> {
        version: VERSION,
        changes,
    })
}

/// The changes of one write, or `Damaged` for anything this build does not
/// read exactly: read, then written again, it must give the same JSON. Serde
/// alone would take an unknown field, a struct as an array by position, a
/// set in any order, or a missing field of an `Option` type as none.
pub fn decode<I: StoredIds>(text: &str) -> Result<Vec<Change<I>>, Damaged> {
    let body: Body<I> = serde_json::from_str(text).map_err(|_| Damaged)?;
    let read: Value = serde_json::from_str(text).map_err(|_| Damaged)?;
    let changes: Vec<_> = body.changes.into_iter().map(|Item(c)| c).collect();
    let again = encode(&changes)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    match again {
        Some(again) if again == read => Ok(changes),
        _ => Err(Damaged),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "I: StoredIds")]
struct Body<I: StoredIds> {
    version: u32,
    changes: Vec<Item<I>>,
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "I: StoredIds")]
struct Item<I: StoredIds>(#[serde(with = "ChangeDef")] Change<I>);

#[derive(Serialize, Deserialize)]
#[serde(remote = "Of", bound = "I: StoredIds")]
struct OfDef<I: StoredIds> {
    profile: I::Profile,
    binding: I::Binding,
}

/// `Vec<Of<I>>`, each as [`OfDef`].
mod ofs {
    use super::*;

    #[derive(Serialize, Deserialize)]
    #[serde(bound = "I: StoredIds")]
    struct One<I: StoredIds>(#[serde(with = "OfDef")] Of<I>);

    pub fn serialize<I: StoredIds, S: Serializer>(ofs: &[Of<I>], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(ofs.iter().cloned().map(One))
    }

    pub fn deserialize<'de, I: StoredIds, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<Of<I>>, D::Error> {
        let ofs = Vec::<One<I>>::deserialize(d)?;
        Ok(ofs.into_iter().map(|One(of)| of).collect())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Confirmation", rename_all = "snake_case")]
enum ConfirmationDef {
    PrincipalChanged,
    UnsavedSettings,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "PlaceholderFailure", rename_all = "snake_case")]
enum PlaceholderFailureDef {
    Dependency,
    CredentialRejected,
    InstanceUnidentified,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "StuckCause", rename_all = "snake_case")]
enum StuckCauseDef {
    Incompatible,
    InstanceUnidentified,
    CredentialRejected,
    AccessRevoked,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Kbs", rename_all = "snake_case")]
enum KbsDef<K> {
    All,
    One(K),
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Rule")]
#[serde(bound = "K: Serialize + DeserializeOwned")]
struct RuleDef<K> {
    epoch: u64,
    #[serde(with = "KbsDef")]
    kbs: Kbs<K>,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Change")]
#[serde(rename_all = "snake_case", bound = "I: StoredIds")]
enum ChangeDef<I: StoredIds> {
    Off,
    On,
    Prompt,
    PromptConfirmed,
    Disable(I::Profile),
    Enable(I::Profile),
    Unselect(I::Profile),
    Select(I::Profile),
    Await {
        profile: I::Profile,
        #[serde(with = "ConfirmationDef")]
        awaited: Confirmation,
    },
    Confirm {
        profile: I::Profile,
        principal: I::Principal,
        #[serde(with = "ConfirmationDef")]
        awaited: Confirmation,
    },
    PlaceholderFailed {
        profile: I::Profile,
        #[serde(with = "PlaceholderFailureDef")]
        failure: PlaceholderFailure,
    },
    PlaceholderCleared {
        profile: I::Profile,
        #[serde(with = "PlaceholderFailureDef")]
        failure: PlaceholderFailure,
    },
    Remove(I::Profile),
    Retire(#[serde(with = "OfDef")] Of<I>),
    Bind {
        #[serde(with = "OfDef")]
        of: Of<I>,
        identity: I::Identity,
        generation: u64,
        confirmed: Option<I::Principal>,
    },
    Generation {
        #[serde(with = "OfDef")]
        of: Of<I>,
        value: u64,
    },
    Servable(#[serde(with = "OfDef")] Of<I>),
    Logout(#[serde(with = "OfDef")] Of<I>),
    Relogin(#[serde(with = "OfDef")] Of<I>),
    Revoked(#[serde(with = "OfDef")] Of<I>),
    Stuck {
        #[serde(with = "OfDef")]
        of: Of<I>,
        #[serde(with = "StuckCauseDef")]
        cause: StuckCause,
    },
    Unstuck {
        #[serde(with = "OfDef")]
        of: Of<I>,
        #[serde(with = "StuckCauseDef")]
        cause: StuckCause,
    },
    RunUnbound {
        run: I::Run,
        profiles: Vec<I::Profile>,
    },
    RunBound {
        run: I::Run,
        #[serde(with = "ofs")]
        bindings: Vec<Of<I>>,
        candidates: Vec<(I::Profile, I::Identity)>,
    },
    RunBackfilled {
        run: I::Run,
        #[serde(with = "OfDef")]
        of: Of<I>,
    },
    Placed {
        #[serde(with = "OfDef")]
        of: Of<I>,
        #[serde(with = "RuleDef")]
        rule: Rule<I::Kb>,
    },
    Questioned {
        #[serde(with = "OfDef")]
        of: Of<I>,
        kbs: BTreeSet<I::Kb>,
        version: u64,
        timeouts: Option<u8>,
    },
    Released {
        #[serde(with = "OfDef")]
        of: Of<I>,
        kbs: BTreeSet<I::Kb>,
        version: u64,
    },
    TimedOut {
        #[serde(with = "OfDef")]
        of: Of<I>,
        count: u8,
    },
    TimeoutsCleared {
        #[serde(with = "OfDef")]
        of: Of<I>,
        from: u8,
    },
    TimeoutsRestored {
        #[serde(with = "OfDef")]
        of: Of<I>,
        count: u8,
    },
    Tombstone {
        #[serde(with = "OfDef")]
        of: Of<I>,
        kb: I::Kb,
    },
    TombstoneWithdrawn {
        #[serde(with = "OfDef")]
        of: Of<I>,
        kb: I::Kb,
    },
    Seen {
        #[serde(with = "OfDef")]
        of: Of<I>,
        kbs: BTreeSet<I::Kb>,
    },
    Unseen {
        #[serde(with = "OfDef")]
        of: Of<I>,
        kbs: BTreeSet<I::Kb>,
    },
    Started,
    Shutdown,
}

#[cfg(test)]
mod tests;
