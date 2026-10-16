#[cfg(unix)]
pub mod parser {
    use std::{os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

    use ocr_worker::ProcessingError;
    use tempfile::TempDir;
    use tokio::{task::JoinHandle, time::timeout};
    use tokio_util::sync::CancellationToken;

    pub struct ParserHarness {
        _root: TempDir,
        socket_path: PathBuf,
        shutdown: CancellationToken,
        task: Option<JoinHandle<Result<(), ProcessingError>>>,
    }

    impl ParserHarness {
        pub async fn start() -> Self {
            let root = tempfile::tempdir().unwrap();
            let socket_directory = root.path().join("socket");
            let scratch_directory = root.path().join("scratch");
            std::fs::create_dir(&socket_directory).unwrap();
            std::fs::create_dir(&scratch_directory).unwrap();
            std::fs::set_permissions(&socket_directory, std::fs::Permissions::from_mode(0o2710))
                .unwrap();
            std::fs::set_permissions(&scratch_directory, std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let socket_path = socket_directory.join("parser.sock");
            let shutdown = CancellationToken::new();
            let service_path = socket_path.clone();
            let service_scratch = scratch_directory.clone();
            let service_shutdown = shutdown.clone();
            let mut task = tokio::spawn(async move {
                ocr_worker::parser_service::serve(
                    &service_path,
                    &service_scratch,
                    ocr_worker::parser_service::ServiceLimits::default(),
                    service_shutdown,
                )
                .await
            });
            timeout(Duration::from_secs(2), async {
                while !socket_path.exists() {
                    if task.is_finished() {
                        match (&mut task).await {
                            Ok(Ok(())) => panic!("parser fixture stopped before bind"),
                            Ok(Err(error)) => panic!("parser fixture failed: {error}"),
                            Err(error) => panic!("parser fixture task failed: {error}"),
                        }
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("parser fixture did not bind its Unix socket");
            Self {
                _root: root,
                socket_path,
                shutdown,
                task: Some(task),
            }
        }

        pub fn socket_path(&self) -> PathBuf {
            self.socket_path.clone()
        }
    }

    impl Drop for ParserHarness {
        fn drop(&mut self) {
            self.shutdown.cancel();
            if let Some(task) = self.task.take() {
                task.abort();
            }
        }
    }
}
