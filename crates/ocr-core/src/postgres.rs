//! Credential-safe PostgreSQL startup options.
//!
//! SQLx 0.8 logs unknown URL query keys **and their values** while parsing,
//! before statement logging can be disabled. Validate keys first with `url`,
//! then discard all driver parse errors and disable statement/slow-query logs.
//! Only fixed error variants escape this module. Normal PostgreSQL URI aliases,
//! encoded Unix socket hosts, query aliases and `options[key]` remain supported.
//! Connection failures must also be redacted by callers: server messages and
//! certificate paths can otherwise become startup diagnostics.

use std::str::FromStr;

use sqlx::{ConnectOptions, postgres::PgConnectOptions};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ConnectionConfigError {
    #[error("DATABASE_URL must be a valid PostgreSQL URI (details redacted)")]
    InvalidUri,
    #[error("DATABASE_URL contains an unsupported connection parameter (details redacted)")]
    UnsupportedParameter,
    #[error("DATABASE_URL has invalid connection options (details redacted)")]
    InvalidOptions,
}

/// The allowlist matches `sqlx-postgres` 0.8.6 `options/parse.rs`. Revisit when
/// upgrading the driver; an unknown key must fail before SQLx sees its value.
pub fn safe_options(uri: &str) -> Result<PgConnectOptions, ConnectionConfigError> {
    if uri.len() > crate::secrets::MAX_SECRET_BYTES || uri.chars().any(char::is_control) {
        return Err(ConnectionConfigError::InvalidUri);
    }
    let parsed = url::Url::parse(uri).map_err(|_| ConnectionConfigError::InvalidUri)?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql") || parsed.fragment().is_some() {
        return Err(ConnectionConfigError::InvalidUri);
    }
    for (key, _) in parsed.query_pairs() {
        let known = matches!(
            key.as_ref(),
            "sslmode"
                | "ssl-mode"
                | "sslrootcert"
                | "ssl-root-cert"
                | "ssl-ca"
                | "sslcert"
                | "ssl-cert"
                | "sslkey"
                | "ssl-key"
                | "statement-cache-capacity"
                | "host"
                | "hostaddr"
                | "port"
                | "dbname"
                | "user"
                | "password"
                | "application_name"
                | "options"
        ) || key
            .strip_prefix("options[")
            .and_then(|value| value.strip_suffix(']'))
            .is_some_and(|name| !name.is_empty());
        if !known {
            return Err(ConnectionConfigError::UnsupportedParameter);
        }
    }
    PgConnectOptions::from_str(uri)
        .map(ConnectOptions::disable_statement_logging)
        .map_err(|_| ConnectionConfigError::InvalidOptions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
        time::Duration,
    };

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn invalid_options_and_unknown_parameter_values_cannot_reach_error_or_trace() {
        let output = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .without_time()
            .with_writer(output.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            // Control proves SQLx's previous parser-warning path would be seen.
            tracing::warn!("capture-control");
            for uri in [
                "postgres://fixture:fixture@localhost/fixture?sslmode=A03_PARSE_SENTINEL",
                "postgres://fixture:fixture@localhost/fixture?password_typo=A03_UNKNOWN_SENTINEL",
                "postgres://fixture:fixture@localhost/fixture?options[broken=A03_BRACKET_SENTINEL",
            ] {
                let error = safe_options(uri).unwrap_err();
                let description = format!("{error:?} {error}");
                assert!(!description.contains("SENTINEL"));
                assert!(!description.contains("fixture"));
                assert!(std::error::Error::source(&error).is_none());
            }
        });
        let trace = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(trace.contains("capture-control"));
        assert!(!trace.contains("SENTINEL"));
        assert!(!trace.contains("password_typo"));
        assert!(!trace.contains("ignoring unrecognized connect parameter"));
    }

    #[test]
    fn documented_uri_forms_and_every_supported_driver_alias_are_preserved() {
        for uri in [
            "postgres://fixture:encoded%40password@localhost:5432/fixture",
            "postgresql://fixture:fixture@localhost/fixture",
            "postgres://%2Fvar%2Frun%2Fpostgresql/fixture",
            "postgres:///fixture?host=%2Fvar%2Frun%2Fpostgresql",
            "postgres://fixture:fixture@localhost/fixture?user=other&password=other&dbname=other&application_name=ocr&hostaddr=127.0.0.1&port=5432&statement-cache-capacity=64",
            "postgres://fixture:fixture@localhost/fixture?options=-c%20synchronous_commit%3Doff&options[search_path]=public&options[lock_timeout]=2s",
        ] {
            assert!(safe_options(uri).is_ok());
        }
        for key in ["sslmode", "ssl-mode"] {
            assert!(
                safe_options(&format!(
                    "postgres://fixture:fixture@localhost/fixture?{key}=verify-full"
                ))
                .is_ok()
            );
        }
        for key in [
            "sslrootcert",
            "ssl-root-cert",
            "ssl-ca",
            "sslcert",
            "ssl-cert",
            "sslkey",
            "ssl-key",
        ] {
            assert!(safe_options(&format!("postgres://fixture:fixture@localhost/fixture?{key}=%2Ffixture%2Fcertificate.pem")).is_ok());
        }
    }

    #[test]
    fn invalid_uri_and_ignored_fragments_fail_without_retaining_input() {
        for uri in [
            "A03_INVALID_URI_SENTINEL",
            "mysql://fixture:fixture@localhost/fixture",
            "postgres://fixture:fixture@localhost/fixture#A03_FRAGMENT_SENTINEL",
            "postgres://fixture:fixture@localhost/fixture?port=A03_PORT_SENTINEL",
        ] {
            let error = safe_options(uri).unwrap_err();
            assert!(!format!("{error:?} {error}").contains("SENTINEL"));
        }
    }

    #[tokio::test]
    async fn connection_failure_redacts_server_message_and_stays_bounded() {
        use sqlx::Connection;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!(
            "postgres://fixture:fixture@{}/fixture?sslmode=disable",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let length = stream.read_u32().await.unwrap();
                assert!((8..=4096).contains(&length));
                let mut startup = vec![0; length as usize - 4];
                stream.read_exact(&mut startup).await.unwrap();
                let payload = b"SFATAL\0VFATAL\0C28000\0MA03_UPSTREAM_CREDENTIAL_SENTINEL\0\0";
                stream.write_u8(b'E').await.unwrap();
                stream.write_u32(payload.len() as u32 + 4).await.unwrap();
                stream.write_all(payload).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        let driver_error = tokio::time::timeout(
            Duration::from_secs(2),
            sqlx::PgConnection::connect_with(&safe_options(&uri).unwrap()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(
            driver_error
                .to_string()
                .contains("A03_UPSTREAM_CREDENTIAL_SENTINEL")
        );
        let error =
            tokio::time::timeout(Duration::from_secs(2), crate::store::Store::connect(&uri))
                .await
                .unwrap()
                .err()
                .expect("connection must fail");
        assert!(matches!(error, crate::store::StoreError::ConnectionSetup));
        assert!(!format!("{error:?} {error}").contains("SENTINEL"));
        assert!(std::error::Error::source(&error).is_none());
        server.await.unwrap();
    }
}
