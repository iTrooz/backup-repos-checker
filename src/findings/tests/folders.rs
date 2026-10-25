//! Folder discovery, literal-prefix matching, and listing failures.

use super::*;

/// Display recursive counts only at visible leaves, excluding directory markers.
#[tokio::test]
async fn displayed_leaves_count_files_recursively() {
    let storage = TestStorage {
        files: [
            "group/repo/a",
            "group/repo/deep/b",
            "group/repo/marker/",
            "unknown/a",
            "unknown/deep/b",
            "unknown/deep/c",
            "unknown/marker/",
            "empty/",
        ]
        .into_iter()
        .map(|path| (path.to_owned(), vec![]))
        .collect(),
        reads: 0.into(),
        fail_reads: true,
    };
    let repositories = [
        RepositoryLocation {
            prefix: "group/repo",
            content_check: None,
            max_size: None,
            silence_checks: &[],
        },
        RepositoryLocation {
            prefix: "group/missing",
            content_check: None,
            max_size: None,
            silence_checks: &[],
        },
    ];
    let checks = Selection::from_kinds(&[crate::findings::selection::Kind::Folders]);

    let tree = inspect(&storage, &repositories, checks).await.unwrap();

    for color in [false, true] {
        let output = tree.render("s3://test", Utc::now(), 7, color, checks);
        assert!(output.contains("repo  [folders: found] (2 files, 0 B)"));
        assert!(output.contains("missing  [folders: not found] (0 files, 0 B)"));
        assert!(output.contains("unknown  [folders: unconfigured] (3 files, 0 B)"));
        assert!(output.contains("empty  [folders: unconfigured] (0 files, 0 B)"));
        assert!(!output.contains("group ("));
        assert!(!output.contains("deep"));
    }

    let repositories = [RepositoryLocation {
        prefix: "",
        content_check: None,
        max_size: None,
        silence_checks: &[],
    }];

    let tree = inspect(&storage, &repositories, checks).await.unwrap();
    assert_eq!(
        tree.render("s3://test", Utc::now(), 7, false, checks),
        "s3://test  [folders: found] (5 files, 0 B)\n"
    );
}

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
        max_size: None,
        silence_checks: &[],
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
        max_size: None,
        silence_checks: &[],
    }];

    assert!(
        inspect(&Broken, &repositories, Selection::default())
            .await
            .is_err()
    );
}

/// Collapse unconfigured branches without hiding configured groups or altering reports.
#[tokio::test]
async fn unconfigured_folders_hide_children_only_in_tree_output() {
    let storage = TestStorage {
        files: [
            ("group/repo/private/file".to_owned(), vec![]),
            ("group/unconfigured/deep/file".to_owned(), vec![]),
            ("root-unconfigured/nested/file".to_owned(), vec![]),
        ]
        .into(),
        reads: 0.into(),
        fail_reads: true,
    };
    let repositories = [
        RepositoryLocation {
            prefix: "group/repo",
            content_check: None,
            max_size: None,
            silence_checks: &[],
        },
        RepositoryLocation {
            prefix: "group/missing",
            content_check: None,
            max_size: None,
            silence_checks: &[],
        },
    ];
    let checks = Selection::default();

    let tree = inspect(&storage, &repositories, checks).await.unwrap();

    for color in [false, true] {
        let output = tree.render("s3://test", Utc::now(), 7, color, checks);
        assert!(output.contains("group"));
        assert!(output.contains("repo  [folders: found]  [age: latest is "));
        assert!(output.contains("missing  [folders: not found]"));
        assert!(output.contains("unconfigured"));
        assert!(output.contains("root-unconfigured"));
        assert_eq!(output.lines().count(), 6);
        for hidden in ["private", "deep", "nested"] {
            assert!(!output.contains(hidden), "{output}");
        }

        if color {
            assert!(output.contains("\x1b[31munconfigured"));
            assert!(output.contains("\x1b[31mroot-unconfigured"));
            assert!(!output.contains("\x1b[31mgroup"));
        }
    }

    assert!(
        tree.children["root-unconfigured"]
            .children
            .contains_key("nested")
    );

    let issues = tree.issues("s3://test", Utc::now(), 7, checks);
    assert!(
        issues
            .iter()
            .any(|issue| issue == "s3://test/group/unconfigured/deep: folders: unconfigured")
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue == "s3://test/root-unconfigured/nested: folders: unconfigured")
    );
}
