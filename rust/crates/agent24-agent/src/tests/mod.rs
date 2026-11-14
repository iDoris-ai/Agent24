#![allow(clippy::unwrap_used, clippy::expect_used)]

mod session_memory_tests {
    include!("../session_memory_tests.rs");
}
mod hybrid_recall_tests {
    include!("../hybrid_recall_tests.rs");
}
mod session_failure_tests {
    include!("../session_failure_tests.rs");
}
mod memory_receipt_tests {
    include!("../memory_receipt_tests.rs");
}
mod snapshot_origin_tests {
    include!("../snapshot_origin_tests.rs");
}
mod source_send_gate_tests {
    include!("../source_send_gate_tests.rs");
}
mod send_trim_tests {
    include!("../send_trim_tests.rs");
}
mod skill_inject_tests {
    include!("../skill_inject_tests.rs");
}

include!("profile_and_normalize.rs");
include!("harness.rs");
include!("workspace.rs");
include!("session_flow.rs");
include!("inbound.rs");
include!("lifecycle.rs");
