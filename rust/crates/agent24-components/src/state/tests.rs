#![allow(clippy::unwrap_used)]
use super::*;

const ALL: [ComponentState; 10] = [
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
];
const FORMS: [Form; 6] = [
    Form::Resident,
    Form::Sidecar,
    Form::Cli,
    Form::Attach,
    Form::Remote,
    Form::Mcp,
];

#[test]
fn every_table_row_is_allowed_for_the_full_forms_and_nothing_else_is() {
    assert_eq!(TABLE.len(), 21, "the ADR table has 21 edges");
    for form in [Form::Resident, Form::Sidecar] {
        for from in ALL {
            for to in ALL {
                assert_eq!(
                    form.can_transition(from, to),
                    TABLE.contains(&(from, to)),
                    "{form:?} {from:?}->{to:?}"
                );
            }
        }
    }
}

#[test]
fn every_form_keeps_to_its_subset() {
    for form in FORMS {
        for from in ALL {
            for to in ALL {
                if form.can_transition(from, to) {
                    assert!(
                        form.allows(from) && form.allows(to),
                        "{form:?} {from:?}->{to:?}"
                    );
                }
                assert!(!form.can_transition(from, from));
            }
        }
    }
    let shown = |f: Form| ALL.into_iter().filter(|s| f.allows(*s)).collect::<Vec<_>>();
    assert_eq!(
        shown(Form::Cli),
        [Absent, Installing, Installed, Incompatible, Failed]
    );
    assert_eq!(
        shown(Form::Attach),
        [
            Starting,
            Ready,
            Degraded,
            Stopping,
            Stopped,
            Incompatible,
            Failed
        ]
    );
    assert_eq!(
        shown(Form::Remote),
        [Starting, Ready, Degraded, Stopped, Incompatible, Failed]
    );
    assert_eq!(
        shown(Form::Mcp),
        [Installed, Starting, Ready, Stopped, Failed]
    );
}

#[test]
fn the_prose_edges_of_the_subsets_exist_and_no_others() {
    // attach: disconnect returns to stopped; connecting, handshake, retry from the table.
    assert!(Form::Attach.can_transition(Ready, Stopped));
    assert!(Form::Attach.can_transition(Stopped, Starting));
    assert!(Form::Attach.can_transition(Starting, Ready));
    assert!(
        !Form::Attach.can_transition(Stopped, Ready),
        "must handshake first"
    );
    // remote: unconfigured -> stopped, probe failure -> degraded, connect failure -> failed.
    assert!(Form::Remote.can_transition(Ready, Stopped));
    assert!(Form::Remote.can_transition(Ready, Degraded));
    assert!(Form::Remote.can_transition(Starting, Failed));
    assert!(!Form::Remote.can_transition(Ready, Stopping));
    // mcp
    assert!(Form::Mcp.can_transition(Starting, Ready));
    assert!(Form::Mcp.can_transition(Ready, Stopped));
    assert!(!Form::Mcp.can_transition(Ready, Degraded));
    assert!(!Form::Mcp.can_transition(Absent, Installing));
    // cli
    assert!(Form::Cli.can_transition(Absent, Installing));
    assert!(Form::Cli.can_transition(Installed, Incompatible));
    assert!(Form::Cli.can_transition(Incompatible, Installing));
    assert!(!Form::Cli.can_transition(Installed, Starting));
    // never skipped steps on the full set
    for (from, to) in [
        (Absent, Ready),
        (Installed, Ready),
        (Ready, Stopped),
        (Stopped, Ready),
        (Failed, Ready),
    ] {
        assert!(!Form::Resident.can_transition(from, to), "{from:?}->{to:?}");
    }
}

#[test]
fn incompatible_and_failed_are_not_terminal() {
    for form in [Form::Resident, Form::Sidecar] {
        assert!(form.can_transition(Failed, Starting));
        assert!(form.can_transition(Failed, Installing));
        assert!(form.can_transition(Incompatible, Installing));
    }
}

#[test]
fn the_mapping_table_rows() {
    // SidecarState: same names; healthy -> ready.
    for (s, want) in [
        ("stopped", Stopped),
        ("starting", Starting),
        ("ready", Ready),
        ("healthy", Ready),
        ("degraded", Degraded),
        ("stopping", Stopping),
        ("failed", Failed),
    ] {
        assert_eq!(from_sidecar(s), Some(want), "{s}");
    }
    assert_eq!(from_sidecar("paused"), None);
    // OpenDesignComponentState
    assert_eq!(from_open_design("not-installed"), Some((Absent, None)));
    assert_eq!(from_open_design("paused"), Some((Installing, None)));
    assert_eq!(from_open_design("installed"), Some((Installed, None)));
    assert_eq!(from_open_design("failed"), Some((Failed, None)));
    assert_eq!(
        from_open_design("unavailable"),
        Some((Absent, Some("unavailable")))
    );
    assert_eq!(from_open_design("downloading"), None);
}

#[test]
fn states_serialize_as_the_lowercase_words_of_the_adr() {
    let all = serde_json::to_string(&ALL).unwrap();
    assert_eq!(
        all,
        r#"["absent","installing","installed","starting","ready","degraded","stopping","stopped","incompatible","failed"]"#
    );
    assert!(serde_json::from_str::<ComponentState>(r#""Ready""#).is_err());
    assert_eq!(serde_json::to_string(&Form::Mcp).unwrap(), r#""mcp""#);
}
