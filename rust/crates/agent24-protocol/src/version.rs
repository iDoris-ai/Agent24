//! Wire protocol version shared by agent24d and its shells.
//!
//! Bump [`PROTOCOL_VERSION`] by 1 when the wire changes incompatibly: a field
//! is removed or renamed, a field's meaning changes, or a peer must refuse
//! the other side instead of ignoring the difference. Adding an optional
//! field that an old peer can skip does not bump this.

/// Protocol this build speaks. Shells compare it with the `protocol_version`
/// advertised by `GET /api/v1/health` and refuse to attach when the two
/// differ. A version stored in `daemon.json` is not that decision.
pub const PROTOCOL_VERSION: u32 = 1;

/// Whether `daemon` can serve a client that speaks `client`.
///
/// `None` is an old daemon that never wrote the field. A mismatch names both
/// versions. The same rules are mirrored in the desktop shell; both sides
/// read `protocol/fixtures/protocol-compatibility.json`.
pub fn protocol_compatibility(daemon: Option<u32>, client: u32) -> Result<(), String> {
    match daemon {
        None => Err("daemon 版本过旧，请升级".to_owned()),
        Some(daemon_version) if daemon_version == client => Ok(()),
        Some(daemon_version) => Err(format!(
            "协议版本不兼容：daemon 协议版本 {daemon_version}，客户端协议版本 {client}"
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct CompatibilityFixture {
        client_protocol_version: u32,
        cases: Vec<CompatibilityCase>,
    }

    #[derive(Debug, Deserialize)]
    struct CompatibilityCase {
        name: String,
        daemon: Option<u32>,
        client: u32,
        compatible: bool,
        reason: Option<String>,
    }

    #[test]
    fn shared_fixture_matches_the_rust_rule() {
        let fixture: CompatibilityFixture = serde_json::from_str(include_str!(
            "../../../../protocol/fixtures/protocol-compatibility.json"
        ))
        .unwrap();
        assert_eq!(
            PROTOCOL_VERSION, fixture.client_protocol_version,
            "PROTOCOL_VERSION drifted from the shared fixture"
        );
        assert!(!fixture.cases.is_empty());
        for case in fixture.cases {
            let got = protocol_compatibility(case.daemon, case.client);
            if case.compatible {
                assert!(got.is_ok(), "{} should be compatible", case.name);
            } else {
                assert_eq!(
                    got.unwrap_err(),
                    case.reason
                        .unwrap_or_else(|| panic!("{} is missing reason", case.name)),
                    "{}",
                    case.name
                );
            }
        }
    }
}
