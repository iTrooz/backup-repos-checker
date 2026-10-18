//! Folder discovery, literal-prefix matching, and listing failures.

use super::*;

/// Similar prefixes and bare objects must not establish repository existence.
#[tokio::test]
async fn exact_object_and_similar_prefix_do_not_establish_existence() {
    let storage = TestStorage {
        files: [
            ("repo".to_owned(), vec![]),
            ("repository/file".to_owned(), vec![]),
        ]
        .into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let repositories = [RepositoryLocation {
        prefix: "repo",
        content_check: None,
    }];
    assert!(
        inspect(&storage, &repositories, Selection::default())
            .await
            .unwrap()
            .failed(Selection::default())
    );
}

/// Listing failures propagate rather than claiming repositories are missing.
#[tokio::test]
async fn listing_errors_propagate() {
    /// A backend that fails during streaming rather than listing initialization.
    struct Broken;
    #[async_trait]
    impl Storage for Broken {
        /// No object reads are expected after a listing failure.
        async fn read_prefix(&self, _path: &str, _length: usize) -> Result<Vec<u8>> {
            unreachable!()
        }
        /// Produce a storage failure instead of an object entry.
        async fn list(&self) -> Result<BoxStream<'_, Result<FileEntry>>> {
            Ok(Box::pin(stream::iter([Err(anyhow::anyhow!(
                "listing failed"
            ))])))
        }
    }
    let repositories = [RepositoryLocation {
        prefix: "repo",
        content_check: None,
    }];
    assert!(
        inspect(&Broken, &repositories, Selection::default())
            .await
            .is_err()
    );
}

/// An entire-bucket repository renders without exposing its descendants.
#[tokio::test]
async fn bucket_root_repository_collapses_everything() {
    let storage = TestStorage {
        files: [("deep/file".to_owned(), vec![])].into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let repositories = [RepositoryLocation {
        prefix: "",
        content_check: None,
    }];
    let checks = Selection::default();
    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert!(!tree.failed(checks));
    assert!(tree.issues("s3://test", Utc::now(), 7, checks).is_empty());
    assert_eq!(
        tree.render("s3://test", Utc::now(), 7, false, checks)
            .lines()
            .count(),
        1
    );
}
