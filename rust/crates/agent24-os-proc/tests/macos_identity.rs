#![cfg(target_os = "macos")]

#[test]
#[ignore = "requires macOS Security.framework access outside the sandbox"]
fn unsigned_test_process_is_rejected() {
    assert!(
        agent24_os_proc::verify_code_identity(
            std::process::id() as i32,
            "HYAP444FN9",
            &["agent24"],
        )
        .is_err()
    );
}
