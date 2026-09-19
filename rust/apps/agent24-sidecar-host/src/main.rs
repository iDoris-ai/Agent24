//! Entry point reserved for the private sidecar protocol dispatcher.

fn main() {
    // Keep the executable separate from agent24d so the private sidecar
    // protocol has one host. Dispatch is layered on the owner primitive.
    let _protocol_version = agent24_sidecar_host_protocol::PROTOCOL_VERSION;
}
