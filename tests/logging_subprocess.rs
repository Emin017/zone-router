/// Subprocess test verifying the real `init_tracing()` path rejects double init.
///
/// The inner test `double_init_tracing_panics` calls `init_tracing()` twice and
/// the `#[should_panic]` attribute confirms the second call panics. It is
/// isolated in a subprocess because `.init()` sets a global subscriber that
/// poisons the rest of the suite.
use std::process::Command;

#[test]
fn double_init_tracing_panics_in_subprocess() {
    // Run the inner test as a subprocess so the global subscriber doesn't
    // interfere with other tests. The inner test uses #[should_panic], so
    // the subprocess reports success when the panic is caught correctly.
    let output = Command::new(env!("CARGO"))
        .args([
            "test",
            "--lib",
            "--",
            "--exact",
            "logging::tests::double_init_tracing_panics",
        ])
        .output()
        .expect("failed to spawn cargo test subprocess");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("should panic ... ok"),
        "double init should panic and be caught by #[should_panic]\nstdout: {stdout}"
    );
}
