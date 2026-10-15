//! Real process regression: URL parsing fails before connecting/listening.
#[test]
fn invalid_database_urls_never_print_query_values_at_trace_level() {
    for suffix in [
        "sslmode=A03_PROCESS_PARSE_SENTINEL",
        "password_typo=A03_PROCESS_UNKNOWN_SENTINEL",
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_ocr-api"))
            .env_clear()
            .env(
                "DATABASE_URL",
                format!("postgres://fixture:fixture@127.0.0.1:1/fixture?{suffix}"),
            )
            .env("OCR_API_TOKEN", "synthetic-api-token-for-startup-test")
            .env("RUST_LOG", "trace")
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!stdout.contains("SENTINEL"));
        assert!(!stderr.contains("SENTINEL"));
        assert!(!stdout.contains("password_typo"));
        assert!(!stderr.contains("password_typo"));
        assert!(stderr.contains("database connection setup failed (details redacted)"));
    }
}
