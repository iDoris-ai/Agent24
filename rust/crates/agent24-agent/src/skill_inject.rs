//! Skill admission and injection (loader v1, slice SL-5; plan §4.6-§4.8).
//!
//! Admission runs before the run row exists. It reads `skill_pointer` only:
//! no pointer is [`SkillAdmissionError::NotActive`] and no revision is read.
//! The pin itself is written by the store in the transaction that inserts the
//! run, from the pointer as it is then. Injection (`execute`) reads the bytes
//! through the pin, never through the pointer, so a pointer that moves later
//! does not change a run. The origin used for the label comes from the stored
//! revision row; text inside the rendered segment is never evidence of it.

use std::collections::BTreeMap;

use agent24_store::{PinnedSkill, SkillOrigin};
use async_trait::async_trait;

/// One component that provides a capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillProvider {
    pub component: String,
    pub version: Option<String>,
}

impl SkillProvider {
    fn label(&self) -> String {
        match &self.version {
            Some(version) => format!("{}@{version}", self.component),
            None => self.component.clone(),
        }
    }
}

/// Who can currently provide a capability. The daemon supplies this from its
/// component view; the agent crate does not know about components.
#[async_trait]
pub trait SkillProviderSource: Send + Sync {
    /// Components that provide `capability` right now (ready, and not marked
    /// unavailable by their own report).
    async fn providers(&self, capability: &str) -> Vec<SkillProvider>;
    /// The component the registry names as the provider, for the message when
    /// there are none.
    fn registered_provider(&self, _capability: &str) -> Option<String> {
        None
    }
}

/// Why a run with a `skill_id` was refused. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillAdmissionError {
    /// The inbound and self-wake paths, and workspace-bound runs, cannot carry a skill.
    #[error("skill_not_admitted")]
    NotAdmitted { reason: &'static str },
    /// No pointer: not installed, or installed and never activated.
    #[error("skill_not_active")]
    NotActive,
    #[error("skill_capability_missing")]
    CapabilityMissing {
        capability: String,
        name_zh: Option<&'static str>,
        registered_provider: Option<String>,
    },
    #[error("skill_provider_ambiguous")]
    ProviderAmbiguous {
        capability: String,
        components: Vec<String>,
    },
    /// A stored row that admission must be able to trust is not usable. The
    /// caller cannot fix it by changing the request: a 500, not a 4xx.
    #[error("skill state is corrupt: {detail}")]
    Corrupt { detail: &'static str },
}

impl SkillAdmissionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAdmitted { .. } => "skill_not_admitted",
            Self::NotActive => "skill_not_active",
            Self::CapabilityMissing { .. } => "skill_capability_missing",
            Self::ProviderAmbiguous { .. } => "skill_provider_ambiguous",
            Self::Corrupt { .. } => "internal",
        }
    }
}

pub(crate) struct Admitted {
    pub(crate) body_revision_id: String,
    pub(crate) providers_json: String,
}

pub(crate) fn parse_for_origin(
    origin: SkillOrigin,
    markdown: &str,
) -> Result<agent24_skill::SkillManifest, agent24_skill::SkillError> {
    match origin {
        SkillOrigin::User => agent24_skill::parse_skill_markdown(markdown),
        SkillOrigin::Imported => agent24_skill::parse_imported(markdown),
    }
}

/// Read the pointer and resolve providers for the body it names.
pub(crate) async fn admit(
    store: &agent24_store::Store,
    providers: Option<&dyn SkillProviderSource>,
    skill_id: &str,
) -> Result<Admitted, crate::AgentError> {
    let corrupt =
        |detail: &'static str| crate::AgentError::from(SkillAdmissionError::Corrupt { detail });
    let Some(pointer) = store.get_skill_pointer(skill_id).await? else {
        return Err(SkillAdmissionError::NotActive.into());
    };
    let body = store
        .get_skill_revision(&pointer.body_revision_id)
        .await?
        .ok_or_else(|| corrupt("skill pointer names a missing revision"))?;
    let text = String::from_utf8(body.content).map_err(|_| corrupt("skill body is not utf-8"))?;
    let manifest = parse_for_origin(body.origin, &text)
        .map_err(|_| corrupt("stored skill body no longer parses"))?;
    if manifest.content_digest() != body.content_digest {
        return Err(corrupt("stored skill body does not match its digest"));
    }
    let mut bound = BTreeMap::new();
    let mut requires = manifest.requires.clone();
    requires.sort();
    requires.dedup();
    for capability in requires {
        let found = match providers {
            Some(source) => source.providers(&capability).await,
            None => Vec::new(),
        };
        match found.as_slice() {
            [] => {
                return Err(SkillAdmissionError::CapabilityMissing {
                    name_zh: agent24_skill::registered_capability(&capability).map(|c| c.name_zh),
                    registered_provider: providers
                        .and_then(|source| source.registered_provider(&capability)),
                    capability,
                }
                .into());
            }
            [one] => {
                bound.insert(capability, one.label());
            }
            many => {
                let mut components: Vec<String> =
                    many.iter().map(|p| p.component.clone()).collect();
                components.sort();
                return Err(SkillAdmissionError::ProviderAmbiguous {
                    capability,
                    components,
                }
                .into());
            }
        }
    }
    let providers_json =
        serde_json::to_string(&bound).map_err(|_| corrupt("skill providers do not serialize"))?;
    Ok(Admitted {
        body_revision_id: pointer.body_revision_id,
        providers_json,
    })
}

/// The segment for one pinned run, and what its label needs.
pub(crate) struct Injection {
    pub(crate) segment: String,
    pub(crate) skill_id: String,
    pub(crate) origin: SkillOrigin,
    pub(crate) content_digest: String,
}

/// Render the pinned bytes. The pin is checked against its revision by the
/// store; here the bytes must also parse to the pinned digest.
pub(crate) fn render(pinned: &PinnedSkill) -> Result<Injection, String> {
    let text = std::str::from_utf8(&pinned.content).map_err(|_| "skill body is not utf-8")?;
    let manifest = parse_for_origin(pinned.pin.origin, text)
        .map_err(|_| "pinned skill body no longer parses".to_owned())?;
    if manifest.content_digest() != pinned.pin.content_digest {
        return Err("pinned skill body does not match the pinned digest".to_owned());
    }
    let origin = match pinned.pin.origin {
        SkillOrigin::User => agent24_skill::SkillOrigin::User,
        SkillOrigin::Imported => agent24_skill::SkillOrigin::Imported,
    };
    Ok(Injection {
        segment: agent24_skill::render_skill_segment(
            &manifest.body,
            origin,
            &pinned.pin.content_digest,
        ),
        skill_id: pinned.pin.skill_id.clone(),
        origin: pinned.pin.origin,
        content_digest: pinned.pin.content_digest.clone(),
    })
}

/// `true` when the run has a skill pin and no usable `SkillText` label for
/// it, or when either cannot be read. A run without a pin has nothing to
/// check. Only stored rows are read: not the pointer, and nothing is
/// reclassified. The label must carry the pinned digest, and an imported
/// skill's label must not be `Ordinary`.
pub(crate) async fn label_missing(store: &agent24_store::Store, run_id: &str) -> bool {
    use agent24_store::{SkillOrigin as Origin, SourceClassification, SourceKind};
    let pinned = match store.get_run_skill_pin(run_id).await {
        Ok(None) => return false,
        Ok(Some(pinned)) => pinned,
        Err(err) => {
            tracing::warn!(run_id, error = %err, "skill pin unreadable");
            return true;
        }
    };
    let tags = match store.list_run_source_tags(run_id).await {
        Ok(tags) => tags,
        Err(err) => {
            tracing::warn!(run_id, error = %err, "run source tags unreadable");
            return true;
        }
    };
    let id = format!("skill_text:{}", pinned.pin.skill_id);
    !tags.iter().any(|tag| {
        tag.source.kind == SourceKind::SkillText
            && tag.source.source_id == id
            && tag.source.revision_digest.as_deref() == Some(pinned.pin.content_digest.as_str())
            && (pinned.pin.origin != Origin::Imported
                || tag.source.classification != SourceClassification::Ordinary)
    })
}
