//! Tests for the LocalRepoMetadataModel.

#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use crate::entry::{DirectoryEntry, Entry, FileMetadata};
    use crate::file_tree_store::{FileTreeEntry, FileTreeEntryState, FileTreeState};
    use crate::local_model::{
        GetContentsArgs, IndexedRepoState, LocalRepoMetadataModel, RepoUpdate,
        RepositoryMetadataEvent,
    };
    use crate::repositories::DetectedRepositories;
    use crate::watcher::DirectoryWatcher;
    use futures::channel::oneshot;
    use futures::executor::block_on;
    use ignore::gitignore::Gitignore;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::Duration;
    use virtual_fs::{Stub, VirtualFS};
    use mnemos_util::standardized_path::StandardizedPath;
    use mnemos_ui::r#async::FutureExt as _;
    use mnemos_ui::App;

    impl LocalRepoMetadataModel {
        fn new_for_test() -> Self {
            Self {
                repositories: HashMap::new(),
                lazy_loaded_paths: Default::default(),
                #[cfg(feature = "local_fs")]
                watcher: Default::default(),
                emit_incremental_updates: false,
            }
        }
    }

    #[test]
    fn test_get_repo_contents() {
        VirtualFS::test("repo_contents_test", |dirs, mut vfs| {
            let test_repo = dirs.tests().join("test_repo");

            // Create a test repository structure using VirtualFS with .git directory
            vfs.mkdir("test_repo/.git/objects")
                .mkdir("test_repo/subdir")
                .with_files(vec![
                    Stub::FileWithContent("test_repo/.git/HEAD", "ref: refs/heads/main"),
                    Stub::FileWithContent(
                        "test_repo/.git/config",
                        "[core]\n\trepositoryformatversion = 0",
                    ),
                    Stub::FileWithContent("test_repo/file1.txt", "content1"),
                    Stub::FileWithContent("test_repo/subdir/file2.rs", "content2"),
                    Stub::FileWithContent("test_repo/subdir/file3.py", "content3"),
                    Stub::FileWithContent("test_repo/file4.md", "content4"),
                    Stub::FileWithContent("test_repo/.gitignore", ""),
                ]);

            // Create a mock file tree structure
            let file1 = Entry::File(FileMetadata::new(test_repo.join("file1.txt"), false));
            let file2 = Entry::File(FileMetadata::new(test_repo.join("subdir/file2.rs"), false));
            let file3 = Entry::File(FileMetadata::new(test_repo.join("subdir/file3.py"), false));
            let file4 = Entry::File(FileMetadata::new(test_repo.join("file4.md"), false));

            let subdir = Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_from_local(&test_repo.join("subdir")).unwrap(),
                children: vec![file2, file3],
                ignored: false,
                loaded: true,
            });

            let root = Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_from_local(&test_repo).unwrap(),
                children: vec![file1, subdir, file4],
                ignored: false,
                loaded: true,
            });

            let (gitignore, _) = Gitignore::new(test_repo.join(".gitignore"));

            App::test((), |mut app| async move {
                // Create RepoWatcher and get Repository handle through it
                let repo_watcher = app.add_singleton_model(DirectoryWatcher::new);
                let repo_handle = repo_watcher.update(&mut app, |repo_watcher, ctx| {
                    repo_watcher
                        .add_directory(
                            StandardizedPath::from_local_canonicalized(&test_repo).unwrap(),
                            ctx,
                        )
                        .unwrap()
                });
                let state = FileTreeState::new(root, vec![gitignore], Some(repo_handle));

                let model_handle = app.add_model(|_| LocalRepoMetadataModel::new_for_test());

                model_handle.update(&mut app, |model, _ctx| {
                    // Use the CanonicalizedPath as the key
                    let canonical_key =
                        StandardizedPath::from_local_canonicalized(&test_repo).unwrap();
                    model
                        .repositories
                        .insert(canonical_key, IndexedRepoState::Indexed(state));
                });

                // Test getting all files
                model_handle.read(&app, |model, _ctx| {
                    let args = GetContentsArgs {
                        include_folders: false,
                        include_ignored: false,
                        filter: None,
                    };
                    let files = model
                        .get_repo_contents(
                            &StandardizedPath::from_local_canonicalized(&test_repo).unwrap(),
                            args,
                        )
                        .unwrap();

                    // Should have 4 files total (file1.txt, file2.rs, file3.py, file4.md)
                    assert_eq!(files.len(), 4);

                    // Test with non-existent repository
                    let non_existent = StandardizedPath::try_new("/non_existent_repo").unwrap();
                    let args = GetContentsArgs {
                        include_folders: false,
                        include_ignored: false,
                        filter: None,
                    };
                    let non_existent_result = model.get_repo_contents(&non_existent, args);
                    assert!(non_existent_result.is_none());
                });
            });
        });
    }

    #[cfg(feature = "local_fs")]
    #[test]
    fn test_lazy_loaded_path_registrations_are_refcounted() {
        VirtualFS::test("lazy_loaded_path_refcount", |dirs, mut vfs| {
            vfs.mkdir("shared_dir")
                .with_files(vec![Stub::FileWithContent(
                    "shared_dir/file.txt",
                    "content",
                )]);

            let shared_dir = dirs.tests().join("shared_dir");

            App::test((), |mut app| async move {
                let model_handle = app.add_model(|_| LocalRepoMetadataModel::new_for_test());

                let shared_dir_for_index =
                    StandardizedPath::from_local_canonicalized(&shared_dir).unwrap();
                model_handle.update(&mut app, |model, ctx| {
                    model
                        .index_lazy_loaded_path(&shared_dir_for_index, ctx)
                        .unwrap();
                    model
                        .index_lazy_loaded_path(&shared_dir_for_index, ctx)
                        .unwrap();
                });

                model_handle.read(&app, |model, _ctx| {
                    assert!(model.is_lazy_loaded_path(
                        &StandardizedPath::from_local_canonicalized(&shared_dir).unwrap()
                    ));
                    assert!(model.has_repository(
                        &StandardizedPath::from_local_canonicalized(&shared_dir).unwrap()
                    ));
                });

                let shared_dir_std =
                    StandardizedPath::from_local_canonicalized(&shared_dir).unwrap();

                model_handle.update(&mut app, |model, ctx| {
                    model.remove_lazy_loaded_path(&shared_dir_std, ctx);
                });

                model_handle.read(&app, |model, _ctx| {
                    assert!(model.is_lazy_loaded_path(&shared_dir_std));
                    assert!(model.has_repository(&shared_dir_std));
                });

                model_handle.update(&mut app, |model, ctx| {
                    model.remove_lazy_loaded_path(&shared_dir_std, ctx);
                });

                model_handle.read(&app, |model, _ctx| {
                    assert!(!model.is_lazy_loaded_path(
                        &StandardizedPath::from_local_canonicalized(&shared_dir).unwrap()
                    ));
                    assert!(!model.has_repository(
                        &StandardizedPath::from_local_canonicalized(&shared_dir).unwrap()
                    ));
                });
            });
        });
    }

    #[cfg(feature = "local_fs")]
    #[test]
    fn test_index_directory_upgrades_lazy_loaded_path_to_repo() {
        VirtualFS::test("lazy_loaded_path_upgrade", |dirs, mut vfs| {
            vfs.mkdir("repo/.git/objects")
                .mkdir("repo/src/nested")
                .with_files(vec![
                    Stub::FileWithContent("repo/.git/HEAD", "ref: refs/heads/main"),
                    Stub::FileWithContent(
                        "repo/.git/config",
                        "[core]\n\trepositoryformatversion = 0",
                    ),
                    Stub::FileWithContent("repo/src/nested/main.rs", "fn main() {}\n"),
                ]);

            let repo_root = dirs.tests().join("repo");
            let src_dir = repo_root.join("src");
            let source_file = repo_root.join("src/nested/main.rs");

            App::test((), |mut app| async move {
                let directory_watcher = app.add_singleton_model(DirectoryWatcher::new);
                let repository_handle = directory_watcher.update(&mut app, |watcher, ctx| {
                    watcher
                        .add_directory(
                            StandardizedPath::from_local_canonicalized(&repo_root).unwrap(),
                            ctx,
                        )
                        .unwrap()
                });
                let model_handle = app.add_model(|_| LocalRepoMetadataModel::new_for_test());

                let repo_root_for_index =
                    StandardizedPath::from_local_canonicalized(&repo_root).unwrap();
                model_handle.update(&mut app, |model, ctx| {
                    model
                        .index_lazy_loaded_path(&repo_root_for_index, ctx)
                        .unwrap();
                });

                model_handle.read(&app, |model, _ctx| {
                    assert!(model.is_lazy_loaded_path(
                        &StandardizedPath::from_local_canonicalized(&repo_root).unwrap()
                    ));
                    let Some(IndexedRepoState::Indexed(state)) = model.repository_state(
                        &StandardizedPath::from_local_canonicalized(&repo_root).unwrap(),
                    ) else {
                        panic!("expected indexed lazy-loaded path");
                    };
                    assert!(state
                        .entry
                        .contains(&StandardizedPath::try_from_local(&src_dir).unwrap()));
                    assert!(!state
                        .entry
                        .contains(&StandardizedPath::try_from_local(&source_file).unwrap()));
                });

                let (tx, rx) = oneshot::channel();
                let repo_root_for_event = repo_root.clone();
                let upgrade_completed = Rc::new(RefCell::new(Some(tx)));
                let upgrade_completed_for_event = upgrade_completed.clone();
                app.update(|ctx| {
                    ctx.subscribe_to_model(&model_handle, move |_, event, _ctx| {
                        if matches!(
                            event,
                            RepositoryMetadataEvent::RepositoryUpdated { path }
                                if path.to_local_path().as_ref() == Some(&repo_root_for_event)
                        ) {
                            if let Some(tx) = upgrade_completed_for_event.borrow_mut().take() {
                                let _ = tx.send(());
                            }
                        }
                    });
                });

                model_handle.update(&mut app, |model, ctx| {
                    model.index_directory(repository_handle, ctx).unwrap();
                });
                rx.with_timeout(Duration::from_secs(5))
                    .await
                    .expect("timed out waiting for repo upgrade")
                    .expect("repo upgrade completion sender dropped");

                model_handle.read(&app, |model, _ctx| {
                    assert!(!model.is_lazy_loaded_path(
                        &StandardizedPath::from_local_canonicalized(&repo_root).unwrap()
                    ));
                    let Some(IndexedRepoState::Indexed(state)) = model.repository_state(
                        &StandardizedPath::from_local_canonicalized(&repo_root).unwrap(),
                    ) else {
                        panic!("expected indexed repo after upgrade");
                    };
                    assert!(state
                        .entry
                        .contains(&StandardizedPath::try_from_local(&source_file).unwrap()));
                });
            });
        });
    }

    #[test]
    fn test_get_repo_contents_include_ignored() {
        VirtualFS::test("repo_contents_include_ignored_test", |dirs, mut vfs| {
            let test_repo = dirs.tests().join("test_repo");

            // Create a test repository structure with both ignored and non-ignored files
            vfs.mkdir("test_repo/.git/objects")
                .mkdir("test_repo/src")
                .mkdir("test_repo/target/debug")
                .mkdir("test_repo/node_modules")
                .with_files(vec![
                    Stub::FileWithContent("test_repo/.git/HEAD", "ref: refs/heads/main"),
                    Stub::FileWithContent(
                        "test_repo/.git/config",
                        "[core]\n\trepositoryformatversion = 0",
                    ),
                    Stub::FileWithContent("test_repo/src/main.rs", "fn main() {}"),
                    Stub::FileWithContent("test_repo/README.md", "# Project"),
                    Stub::FileWithContent("test_repo/target/debug/binary", "binary"),
                    Stub::FileWithContent("test_repo/node_modules/package.json", "{}"),
                    Stub::FileWithContent("test_repo/debug.log", "log"),
                    Stub::FileWithContent("test_repo/.gitignore", "*.log\n/target/\nnode_modules/"),
                ]);

            // Create mock file tree with ignored and non-ignored entries
            let main_rs = Entry::File(FileMetadata::new(test_repo.join("src/main.rs"), false));
            let readme = Entry::File(FileMetadata::new(test_repo.join("README.md"), false));
            let debug_log = Entry::File(FileMetadata::new(test_repo.join("debug.log"), true));
            let binary = Entry::File(FileMetadata::new(
                test_repo.join("target/debug/binary"),
                true,
            ));
            let package_json = Entry::File(FileMetadata::new(
                test_repo.join("node_modules/package.json"),
                true,
            ));

            let src_dir = Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_from_local(&test_repo.join("src")).unwrap(),
                children: vec![main_rs],
                ignored: false,
                loaded: true,
            });

            let debug_dir = Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_from_local(&test_repo.join("target/debug")).unwrap(),
                children: vec![binary],
                ignored: true,
                loaded: true,
            });

            let target_dir = Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_from_local(&test_repo.join("target")).unwrap(),
                children: vec![debug_dir],
                ignored: true,
                loaded: true,
            });

            let node_modules_dir = Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_from_local(&test_repo.join("node_modules")).unwrap(),
                children: vec![package_json],
                ignored: true,
                loaded: true,
            });

            let root = Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_from_local(&test_repo).unwrap(),
                children: vec![src_dir, readme, debug_log, target_dir, node_modules_dir],
                ignored: false,
                loaded: true,
            });

            let (gitignore, _) = Gitignore::new(test_repo.join(".gitignore"));

            App::test((), |mut app| async move {
                let repo_watcher = app.add_singleton_model(DirectoryWatcher::new);
                let repo_handle = repo_watcher.update(&mut app, |repo_watcher, ctx| {
                    repo_watcher
                        .add_directory(
                            StandardizedPath::from_local_canonicalized(&test_repo).unwrap(),
                            ctx,
                        )
                        .unwrap()
                });
                let state = FileTreeState::new(root, vec![gitignore], Some(repo_handle));

                let model_handle = app.add_model(|_| LocalRepoMetadataModel::new_for_test());

                model_handle.update(&mut app, |model, _ctx| {
                    let canonical_key =
                        StandardizedPath::from_local_canonicalized(&test_repo).unwrap();
                    model
                        .repositories
                        .insert(canonical_key, IndexedRepoState::Indexed(state));
                });

                // Test with include_ignored = false (should exclude ignored files and directories)
                model_handle.read(&app, |model, _ctx| {
                    let args = GetContentsArgs {
                        include_folders: true,
                        include_ignored: false,
                        filter: None,
                    };
                    let contents = model
                        .get_repo_contents(
                            &StandardizedPath::from_local_canonicalized(&test_repo).unwrap(),
                            args,
                        )
                        .unwrap();

                    let paths: Vec<PathBuf> = contents
                        .iter()
                        .map(|c| match c {
                            crate::RepoContent::File(f) => f.path.to_local_path_lossy(),
                            crate::RepoContent::Directory(d) => d.path.to_local_path_lossy(),
                        })
                        .collect();

                    // Should include non-ignored files and directories
                    assert!(paths.contains(&test_repo.join("src")));
                    assert!(paths.contains(&test_repo.join("src/main.rs")));
                    assert!(paths.contains(&test_repo.join("README.md")));

                    // Should NOT include ignored directories or files
                    assert!(!paths.contains(&test_repo.join("target")));
                    assert!(!paths.contains(&test_repo.join("node_modules")));
                    assert!(!paths.contains(&test_repo.join("debug.log")));
                });

                // Test with include_ignored = true (should include everything)
                model_handle.read(&app, |model, _ctx| {
                    let args = GetContentsArgs {
                        include_folders: true,
                        include_ignored: true,
                        filter: None,
                    };
                    let contents = model
                        .get_repo_contents(
                            &StandardizedPath::from_local_canonicalized(&test_repo).unwrap(),
                            args,
                        )
                        .unwrap();

                    let paths: Vec<PathBuf> = contents
                        .iter()
                        .map(|c| match c {
                            crate::RepoContent::File(f) => f.path.to_local_path_lossy(),
                            crate::RepoContent::Directory(d) => d.path.to_local_path_lossy(),
                        })
                        .collect();

                    // Should include everything
                    assert!(paths.contains(&test_repo.join("src")));
                    assert!(paths.contains(&test_repo.join("target")));
                    assert!(paths.contains(&test_repo.join("target/debug")));
                    assert!(paths.contains(&test_repo.join("node_modules")));
                    assert!(paths.contains(&test_repo.join("src/main.rs")));
                    assert!(paths.contains(&test_repo.join("README.md")));
                    assert!(paths.contains(&test_repo.join("debug.log")));
                    assert!(paths.contains(&test_repo.join("target/debug/binary")));
                    assert!(paths.contains(&test_repo.join("node_modules/package.json")));
                });
            });
        });
    }

    #[test]
    fn test_should_include_path_respects_gitignore() {
        VirtualFS::test("gitignore_test", |dirs, mut fs| {
            let repo_path = dirs.tests();

            // Create directory structure and files using VirtualFS
            fs.mkdir("src")
                .mkdir("target/debug")
                .mkdir("node_modules/package")
                .mkdir("docs")
                .with_files(vec![
                    Stub::FileWithContent("debug.log", "log"),
                    Stub::FileWithContent("target/debug/main", "binary"),
                    Stub::FileWithContent("node_modules/package/index.js", "js"),
                    Stub::FileWithContent(".env", "env"),
                    Stub::FileWithContent("src/main.rs", "rust"),
                    Stub::FileWithContent("README.md", "readme"),
                    Stub::FileWithContent("package.json", "json"),
                    Stub::FileWithContent("docs/guide.md", "guide"),
                    Stub::FileWithContent(".gitignore", "*.log\n/target/\nnode_modules/\n.env"),
                ]);

            let gitignore_path = repo_path.join(".gitignore");

            // Create the gitignore object
            let (gitignore, _) = Gitignore::new(&gitignore_path);
            let gitignores = vec![gitignore];

            // Test files that should be excluded
            let excluded_paths = vec![
                repo_path.join("debug.log"),
                repo_path.join("target").join("debug").join("main"),
                repo_path
                    .join("node_modules")
                    .join("package")
                    .join("index.js"),
                repo_path.join(".env"),
            ];

            for path in excluded_paths {
                assert!(
                    LocalRepoMetadataModel::path_is_ignored(&path, &gitignores),
                    "Path should be excluded by gitignore: {path:?}"
                );
            }

            // Test files that should be included
            let included_paths = vec![
                repo_path.join("src").join("main.rs"),
                repo_path.join("README.md"),
                repo_path.join("package.json"),
                repo_path.join("docs").join("guide.md"),
            ];

            for path in included_paths {
                assert!(
                    !LocalRepoMetadataModel::path_is_ignored(&path, &gitignores),
                    "Path should be included: {path:?}"
                );
            }
        });
    }

    #[test]
    fn test_update_file_tree_entry_respects_gitignore() {
        VirtualFS::test("tree_update_test", |dirs, mut fs| {
            let repo_path = dirs.tests();

            // Create initial directory structure and files
            fs.mkdir("src")
                .with_files(vec![
                    Stub::FileWithContent("src/main.rs", "fn main() {}"),
                    Stub::FileWithContent(".gitignore", "*.log\n/target/"),
                    Stub::FileWithContent("debug.log", "log content"),
                    Stub::FileWithContent("README.md", "# Project"),
                ])
                .mkdir("target");

            let gitignore_path = repo_path.join(".gitignore");
            let (gitignore, _) = Gitignore::new(&gitignore_path);
            let gitignores = vec![gitignore];

            // Create an initial file tree
            let root_entry = Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_from_local(repo_path).unwrap(),
                children: vec![Entry::Directory(DirectoryEntry {
                    path: StandardizedPath::try_from_local(&repo_path.join("src")).unwrap(),
                    children: vec![Entry::File(FileMetadata::new(
                        repo_path.join("src").join("main.rs"),
                        false,
                    ))],
                    ignored: false,
                    loaded: true,
                })],
                ignored: false,
                loaded: true,
            });
            let mut root = FileTreeEntry::from(root_entry);

            // Create files to test adding - some should be ignored
            let log_file = repo_path.join("debug.log");
            let target_dir = repo_path.join("target");
            let readme_file = repo_path.join("README.md");

            // Create update with both ignored and allowed files
            let update = RepoUpdate {
                added: vec![log_file.clone(), readme_file.clone(), target_dir.clone()],
                deleted: vec![],
                moved: HashMap::new(),
            };

            // Compute mutations on the "background thread" then apply on the "main thread".
            let mutations = block_on(LocalRepoMetadataModel::compute_file_tree_mutations(
                &update,
                &gitignores,
                false,
            ));
            LocalRepoMetadataModel::apply_file_tree_mutations(&mut root, mutations, false, false);

            // Verify that only the README.md was added (log file and target dir should be ignored)
            let mut all_paths = Vec::new();
            collect_all_paths(&root, &mut all_paths);

            // Should contain all files
            let readme_std = StandardizedPath::try_from_local(&readme_file).unwrap();
            let log_std = StandardizedPath::try_from_local(&log_file).unwrap();
            let target_std = StandardizedPath::try_from_local(&target_dir).unwrap();
            assert!(all_paths.contains(&readme_std));
            assert!(all_paths.contains(&log_std));
            assert!(all_paths.contains(&target_std));

            // Make sure that the ignored files and folders are marked as ignored.
            assert!(root
                .get(&StandardizedPath::try_from_local(&log_file).unwrap())
                .unwrap()
                .ignored());
            assert!(root
                .get(&StandardizedPath::try_from_local(&target_dir).unwrap())
                .unwrap()
                .ignored());

            // Make sure that the ignored folder is not eagerly loaded.
            assert!(!root
                .get(&StandardizedPath::try_from_local(&target_dir).unwrap())
                .unwrap()
                .loaded());
        });
    }

    #[test]
    fn test_gitignore_patterns_comprehensive() {
        VirtualFS::test("comprehensive_test", |dirs, mut fs| {
            let repo_path = dirs.tests();

            // Create directory structure and files using VirtualFS
            fs.mkdir("target/debug")
                .mkdir("dist")
                .mkdir("build")
                .mkdir("logs")
                .mkdir("node_modules/react")
                .mkdir("vendor")
                .mkdir(".vscode")
                .mkdir(".idea")
                .mkdir("src")
                .mkdir("docs")
                .mkdir("tests")
                .mkdir(".github/workflows");

            // Create a comprehensive .gitignore
            let gitignore_content = r#"
# Build outputs
/target/
/dist/
build/

# Logs
*.log
logs/

# Dependencies
node_modules/
/vendor/

# IDE files
.vscode/
.idea/
*.swp

# Environment
.env
.env.local

# OS files
.DS_Store
Thumbs.db
"#;

            // Create all files
            fs.with_files(vec![
                Stub::FileWithContent("target/debug/main", "binary"),
                Stub::FileWithContent("dist/bundle.js", "js"),
                Stub::FileWithContent("logs/app.log", "log"),
                Stub::FileWithContent("debug.log", "log"),
                Stub::FileWithContent("node_modules/react/index.js", "js"),
                Stub::FileWithContent(".vscode/settings.json", "json"),
                Stub::FileWithContent(".env", "env"),
                Stub::FileWithContent(".DS_Store", "store"),
                Stub::FileWithContent("temp.swp", "swap"),
                Stub::FileWithContent("src/main.rs", "rust"),
                Stub::FileWithContent("README.md", "readme"),
                Stub::FileWithContent("package.json", "json"),
                Stub::FileWithContent("docs/guide.md", "guide"),
                Stub::FileWithContent("tests/integration.rs", "test"),
                Stub::FileWithContent(".github/workflows/ci.yml", "yml"),
                Stub::FileWithContent(".gitignore", gitignore_content),
            ]);

            let gitignore_path = repo_path.join(".gitignore");

            let (gitignore, _) = Gitignore::new(&gitignore_path);
            let gitignores = vec![gitignore];

            // Test various patterns
            let test_cases = vec![
                // Should be ignored
                (repo_path.join("target").join("debug").join("main"), false),
                (repo_path.join("dist").join("bundle.js"), false),
                (repo_path.join("logs").join("app.log"), false),
                (repo_path.join("debug.log"), false),
                (
                    repo_path
                        .join("node_modules")
                        .join("react")
                        .join("index.js"),
                    false,
                ),
                (repo_path.join(".vscode").join("settings.json"), false),
                (repo_path.join(".env"), false),
                (repo_path.join(".DS_Store"), false),
                (repo_path.join("temp.swp"), false),
                // Should be included
                (repo_path.join("src").join("main.rs"), true),
                (repo_path.join("README.md"), true),
                (repo_path.join("package.json"), true),
                (repo_path.join("docs").join("guide.md"), true),
                (repo_path.join("tests").join("integration.rs"), true),
                (
                    repo_path.join(".github").join("workflows").join("ci.yml"),
                    true,
                ),
            ];

            for (path, should_include) in test_cases {
                let actual = !LocalRepoMetadataModel::path_is_ignored(&path, &gitignores);
                assert_eq!(
                    actual, should_include,
                    "Path {path:?} - expected: {should_include}, actual: {actual}"
                );
            }
        });
    }

    #[test]
    fn test_git_directory_exclusion() {
        VirtualFS::test("git_exclusion_test", |dirs, mut fs| {
            let repo_path = dirs.tests();

            // Create .git directory and files using VirtualFS
            fs.mkdir(".git/objects").mkdir("src").with_files(vec![
                Stub::FileWithContent(".git/config", "config"),
                Stub::FileWithContent(".git/objects/abc123", "object"),
                Stub::FileWithContent("src/main.rs", "rust"),
            ]);

            let gitignores = vec![]; // Empty gitignore rules

            // .git directory and its contents should be excluded
            assert!(LocalRepoMetadataModel::path_is_ignored(
                &repo_path.join(".git"),
                &gitignores
            ));
            assert!(LocalRepoMetadataModel::path_is_ignored(
                &repo_path.join(".git").join("config"),
                &gitignores
            ));
            assert!(LocalRepoMetadataModel::path_is_ignored(
                &repo_path.join(".git").join("objects").join("abc123"),
                &gitignores
            ));

            // Regular files should be included
            assert!(!LocalRepoMetadataModel::path_is_ignored(
                &repo_path.join("src").join("main.rs"),
                &gitignores
            ));
        });
    }

    #[test]
    fn test_nested_gitignore_rules() {
        VirtualFS::test("nested_gitignore_test", |dirs, mut fs| {
            let repo_path = dirs.tests();

            // Create nested directory structure and files using VirtualFS
            fs.mkdir("frontend/dist")
                .mkdir("backend/target")
                .mkdir("frontend/src")
                .with_files(vec![
                    Stub::FileWithContent("frontend/dist/bundle.js", "js"),
                    Stub::FileWithContent("backend/target/binary", "bin"),
                    Stub::FileWithContent("frontend/src/main.ts", "ts"),
                    Stub::FileWithContent(".gitignore", "*/dist/\n*/target/"),
                    Stub::FileWithContent("frontend/.gitignore", "!dist/important.js"),
                ]);

            // Create gitignore objects
            let root_gitignore_path = repo_path.join(".gitignore");
            let frontend_gitignore_path = repo_path.join("frontend").join(".gitignore");

            let (root_gitignore, _) = Gitignore::new(&root_gitignore_path);
            let (frontend_gitignore, _) = Gitignore::new(&frontend_gitignore_path);
            let gitignores = vec![root_gitignore, frontend_gitignore];

            // Test that nested gitignore rules are respected
            assert!(LocalRepoMetadataModel::path_is_ignored(
                &repo_path.join("frontend").join("dist").join("bundle.js"),
                &gitignores
            ));
            assert!(LocalRepoMetadataModel::path_is_ignored(
                &repo_path.join("backend").join("target").join("binary"),
                &gitignores
            ));
            assert!(!LocalRepoMetadataModel::path_is_ignored(
                &repo_path.join("frontend").join("src").join("main.ts"),
                &gitignores
            ));
        });
    }

    #[test]
    fn test_ensure_parent_directories_exist() {
        use crate::local_model::LocalRepoMetadataModel;

        // Test case 1: Normal operation - creating nested parent directories
        let root_entry = Entry::Directory(DirectoryEntry {
            path: StandardizedPath::try_new("/test_repo").unwrap(),
            children: vec![Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_new("/test_repo/src").unwrap(),
                children: vec![],
                ignored: false,
                loaded: true,
            })],
            ignored: false,
            loaded: true,
        });
        let mut root = FileTreeEntry::from(root_entry);

        // Try to ensure parent directories exist for a deeply nested path
        LocalRepoMetadataModel::ensure_parent_directories_exist(
            &mut root,
            &StandardizedPath::try_new("/test_repo/src/components/ui/forms").unwrap(),
        );

        // Verify that all intermediate directories were created
        let mut all_paths = Vec::new();
        collect_all_paths(&root, &mut all_paths);

        assert!(all_paths.contains(&StandardizedPath::try_new("/test_repo").unwrap()));
        assert!(all_paths.contains(&StandardizedPath::try_new("/test_repo/src").unwrap()));
        assert!(
            all_paths.contains(&StandardizedPath::try_new("/test_repo/src/components").unwrap())
        );
        assert!(
            all_paths.contains(&StandardizedPath::try_new("/test_repo/src/components/ui").unwrap())
        );
        assert!(all_paths
            .contains(&StandardizedPath::try_new("/test_repo/src/components/ui/forms").unwrap()));

        // Test case 2: Existing directories should not be recreated
        let initial_count = all_paths.len();
        LocalRepoMetadataModel::ensure_parent_directories_exist(
            &mut root,
            &StandardizedPath::try_new("/test_repo/src/components/ui/forms").unwrap(),
        );

        let mut updated_paths = Vec::new();
        collect_all_paths(&root, &mut updated_paths);
        assert_eq!(
            initial_count,
            updated_paths.len(),
            "No new directories should be created when they already exist"
        );

        // Test case 3: Edge case - file exists where directory is expected
        // This tests the edge case documented in the function's comment
        let root_with_file_conflict_entry = Entry::Directory(DirectoryEntry {
            path: StandardizedPath::try_new("/test_repo").unwrap(),
            children: vec![
                // Create a file at the path where we'll try to create a directory
                Entry::File(FileMetadata::from_standardized(
                    StandardizedPath::try_new("/test_repo/conflicting_path").unwrap(),
                    false,
                )),
            ],
            ignored: false,
            loaded: true,
        });
        let mut root_with_file_conflict = FileTreeEntry::from(root_with_file_conflict_entry);

        // Try to create parent directories where a file already exists
        LocalRepoMetadataModel::ensure_parent_directories_exist(
            &mut root_with_file_conflict,
            &StandardizedPath::try_new("/test_repo/conflicting_path/nested/deep").unwrap(),
        );

        // Verify that the function returned early and didn't corrupt the tree
        let mut conflict_paths = Vec::new();
        collect_all_paths(&root_with_file_conflict, &mut conflict_paths);

        // The function should detect the file conflict and return early without creating
        // any nested directories beyond the conflicting file.

        // Should still have the original file
        assert!(conflict_paths
            .contains(&StandardizedPath::try_new("/test_repo/conflicting_path").unwrap()));
        // Should NOT have created nested directories beyond the conflict
        assert!(!conflict_paths
            .contains(&StandardizedPath::try_new("/test_repo/conflicting_path/nested").unwrap()));
        assert!(!conflict_paths.contains(
            &StandardizedPath::try_new("/test_repo/conflicting_path/nested/deep").unwrap()
        ));

        // Verify the conflicting entry is still a file, not a directory
        let conflicting_entry = root_with_file_conflict
            .get(&StandardizedPath::try_new("/test_repo/conflicting_path").unwrap())
            .expect("Conflicting entry should exist");
        assert!(
            matches!(conflicting_entry, FileTreeEntryState::File(_)),
            "Conflicting entry should remain a file"
        );

        {
            // Test case 3b: File conflict at intermediate level
            let root_with_intermediate_conflict_entry = Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_new("/test_repo").unwrap(),
                children: vec![Entry::Directory(DirectoryEntry {
                    path: StandardizedPath::try_new("/test_repo/src").unwrap(),
                    children: vec![
                        // Create a file where we expect a directory
                        Entry::File(FileMetadata::from_standardized(
                            StandardizedPath::try_new("/test_repo/src/components").unwrap(),
                            false,
                        )),
                    ],
                    ignored: false,
                    loaded: true,
                })],
                ignored: false,
                loaded: true,
            });
            let mut root_with_intermediate_conflict =
                FileTreeEntry::from(root_with_intermediate_conflict_entry);

            // Try to create nested directories where an intermediate path has a file conflict
            LocalRepoMetadataModel::ensure_parent_directories_exist(
                &mut root_with_intermediate_conflict,
                &StandardizedPath::try_new("/test_repo/src/components/ui/forms").unwrap(),
            );

            // Verify that the function handled the conflict properly
            let mut intermediate_conflict_paths = Vec::new();
            collect_all_paths(
                &root_with_intermediate_conflict,
                &mut intermediate_conflict_paths,
            );

            // Should still have the original file at components level
            assert!(intermediate_conflict_paths
                .contains(&StandardizedPath::try_new("/test_repo/src/components").unwrap()));

            // Should NOT have created deeper nested directories beyond the conflict
            assert!(!intermediate_conflict_paths
                .contains(&StandardizedPath::try_new("/test_repo/src/components/ui").unwrap()));
            assert!(!intermediate_conflict_paths.contains(
                &StandardizedPath::try_new("/test_repo/src/components/ui/forms").unwrap()
            ));

            // Verify the conflicting entry is still a file, not a directory
            let conflicting_entry = root_with_intermediate_conflict
                .get(&StandardizedPath::try_new("/test_repo/src/components").unwrap())
                .expect("Conflicting entry should exist");
            assert!(
                matches!(conflicting_entry, FileTreeEntryState::File(_)),
                "Conflicting entry should remain a file"
            );

            // Test case 4: Single level directory creation
            let simple_root_entry = Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_new("/simple").unwrap(),
                children: vec![],
                ignored: false,
                loaded: true,
            });
            let mut simple_root = FileTreeEntry::from(simple_root_entry);

            let simple_target = StandardizedPath::try_new("/simple/new_dir").unwrap();
            LocalRepoMetadataModel::ensure_parent_directories_exist(
                &mut simple_root,
                &simple_target,
            );

            let mut simple_paths = Vec::new();
            collect_all_paths(&simple_root, &mut simple_paths);
            assert!(simple_paths.contains(&StandardizedPath::try_new("/simple/new_dir").unwrap()));

            // Test case 5: Target parent is the root itself (edge case)
            let root_target_entry = Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_new("/root").unwrap(),
                children: vec![],
                ignored: false,
                loaded: true,
            });
            let mut root_target = FileTreeEntry::from(root_target_entry);

            // This should not crash or create any new directories
            LocalRepoMetadataModel::ensure_parent_directories_exist(
                &mut root_target,
                &StandardizedPath::try_new("/root").unwrap(),
            );

            let mut root_paths = Vec::new();
            collect_all_paths(&root_target, &mut root_paths);
            assert_eq!(root_paths.len(), 1); // Should only contain the root itself

            // Test case 6: Empty path handling
            let empty_root_entry = Entry::Directory(DirectoryEntry {
                path: StandardizedPath::try_new("/empty").unwrap(),
                children: vec![],
                ignored: false,
                loaded: true,
            });
            let mut empty_root = FileTreeEntry::from(empty_root_entry);

            // Test with a path that has no additional parents to create
            let same_level_target = StandardizedPath::try_new("/empty").unwrap();
            LocalRepoMetadataModel::ensure_parent_directories_exist(
                &mut empty_root,
                &same_level_target,
            );

            let mut empty_paths = Vec::new();
            collect_all_paths(&empty_root, &mut empty_paths);
            assert_eq!(empty_paths.len(), 1); // Should still only contain the root
        }
    }

    /// Helper function to collect all paths in a file tree
    fn collect_all_paths(entry: &FileTreeEntry, paths: &mut Vec<StandardizedPath>) {
        let root_path = entry.root_directory().clone();
        collect_paths_recursive(entry, &root_path, paths);
    }

    fn collect_paths_recursive(
        entry: &FileTreeEntry,
        current_path: &StandardizedPath,
        paths: &mut Vec<StandardizedPath>,
    ) {
        paths.push(current_path.clone());
        if let Some(FileTreeEntryState::Directory(_)) = entry.get(current_path) {
            for child in entry.child_paths(current_path) {
                collect_paths_recursive(entry, child, paths);
            }
        }
    }

    #[test]
    fn test_canonicalized_path_functionality() {
        use mnemos_util::standardized_path::StandardizedPath;
        VirtualFS::test("canonicalized_path_test", |dirs, mut vfs| {
            let repo_path = dirs.tests();

            // Create a directory structure with symlinks
            vfs.mkdir("real_dir/subdir")
                .mkdir("other_dir")
                .with_files(vec![
                    Stub::FileWithContent("real_dir/file.txt", "content"),
                    Stub::FileWithContent("real_dir/subdir/nested.rs", "rust code"),
                ]);

            let real_dir = repo_path.join("real_dir");
            let symlink_dir = repo_path.join("symlinked_dir");
            let relative_path = repo_path.join("./real_dir");

            // Create a symlink to real_dir
            #[cfg(unix)]
            let symlink_created = std::os::unix::fs::symlink(&real_dir, &symlink_dir).is_ok();
            #[cfg(windows)]
            let symlink_created =
                std::os::windows::fs::symlink_dir(&real_dir, &symlink_dir).is_ok();

            if symlink_created {
                // Test that different path representations canonicalize to the same path
                let canonical_real = StandardizedPath::from_local_canonicalized(&real_dir).unwrap();
                let canonical_symlink =
                    StandardizedPath::from_local_canonicalized(&symlink_dir).unwrap();
                let canonical_relative =
                    StandardizedPath::from_local_canonicalized(&relative_path).unwrap();

                // All should point to the same canonical path
                assert_eq!(canonical_real, canonical_symlink);
                assert_eq!(canonical_real, canonical_relative);

                // Test that the canonical path is absolute and resolved
                let local = canonical_real.to_local_path().unwrap();
                assert!(local.is_absolute());
                assert!(!local.to_string_lossy().contains("./"));
            }

            // Test with various input types
            let path_buf = real_dir.clone();
            let path_ref = real_dir.as_path();

            let canonical_from_pathbuf =
                StandardizedPath::from_local_canonicalized(&path_buf).unwrap();
            let canonical_from_path = StandardizedPath::from_local_canonicalized(path_ref).unwrap();

            // All should be equal
            assert_eq!(canonical_from_pathbuf, canonical_from_path);

            // Test conversion to local path
            let canonical = StandardizedPath::from_local_canonicalized(&real_dir).unwrap();
            let local_path = canonical.to_local_path().unwrap();

            // Test internal consistency - compare with dunce-canonicalized version
            let expected_canonical = dunce::canonicalize(&real_dir).unwrap();
            assert_eq!(local_path, expected_canonical);

            // Test error handling for non-existent paths
            let nonexistent = repo_path.join("nonexistent");
            let result = StandardizedPath::from_local_canonicalized(&nonexistent);
            assert!(result.is_err());
        });
    }

    #[test]
    fn test_repository_operations_with_standardized_paths() {
        use mnemos_util::standardized_path::StandardizedPath;

        VirtualFS::test("repo_canonicalized_test", |dirs, mut vfs| {
            let test_root = dirs.tests();

            // Create a real repository directory
            vfs.mkdir("real_repo/src")
                .mkdir("other_location")
                .with_files(vec![
                    Stub::FileWithContent("real_repo/src/main.rs", "fn main() {}"),
                    Stub::FileWithContent("real_repo/.gitignore", "*.log\n/target/"),
                    Stub::FileWithContent("real_repo/README.md", "# Project"),
                ]);

            let real_repo = test_root.join("real_repo");
            let symlink_repo = test_root.join("symlinked_repo");
            let relative_repo = test_root.join("./real_repo");

            // Create symlink to the repo
            #[cfg(unix)]
            let symlink_created = std::os::unix::fs::symlink(&real_repo, &symlink_repo).is_ok();
            #[cfg(windows)]
            let symlink_created =
                std::os::windows::fs::symlink_dir(&real_repo, &symlink_repo).is_ok();

            if symlink_created {
                App::test((), |mut app| async move {
                    let repo_watcher = app.add_singleton_model(DirectoryWatcher::new);
                    let _detected_repo =
                        app.add_singleton_model(|_| DetectedRepositories::default());
                    let model_handle = app.add_model(LocalRepoMetadataModel::new);

                    // Create file tree state for testing
                    let src_file =
                        Entry::File(FileMetadata::new(real_repo.join("src/main.rs"), false));
                    let readme_file =
                        Entry::File(FileMetadata::new(real_repo.join("README.md"), false));
                    let src_dir = Entry::Directory(DirectoryEntry {
                        path: StandardizedPath::try_from_local(&real_repo.join("src")).unwrap(),
                        children: vec![src_file],
                        ignored: false,
                        loaded: true,
                    });
                    let root = Entry::Directory(DirectoryEntry {
                        path: StandardizedPath::try_from_local(&real_repo).unwrap(),
                        children: vec![src_dir, readme_file],
                        ignored: false,
                        loaded: true,
                    });

                    let (gitignore, _) = Gitignore::new(real_repo.join(".gitignore"));
                    let repo_handle = repo_watcher.update(&mut app, |repo_watcher, ctx| {
                        repo_watcher
                            .add_directory(
                                StandardizedPath::from_local_canonicalized(&real_repo).unwrap(),
                                ctx,
                            )
                            .unwrap()
                    });
                    let state = FileTreeState::new(root, vec![gitignore], Some(repo_handle));

                    // Test adding repository using different path representations
                    model_handle.update(&mut app, |model, ctx| {
                        // Add using real path
                        let result1 = model.add_repository_internal(
                            StandardizedPath::from_local_canonicalized(&real_repo).unwrap(),
                            state.clone(),
                            ctx,
                        );
                        assert!(result1.is_ok());

                        // Try to add using symlink path - this should canonicalize to the same path
                        let result2 = model.add_repository_internal(
                            StandardizedPath::from_local_canonicalized(&symlink_repo).unwrap(),
                            state.clone(),
                            ctx,
                        );
                        assert!(result2.is_ok());

                        // Try to add using relative path
                        let result3 = model.add_repository_internal(
                            StandardizedPath::from_local_canonicalized(&relative_repo).unwrap(),
                            state.clone(),
                            ctx,
                        );
                        assert!(result3.is_ok());

                        // Verify that only one repository entry exists (all paths canonicalized to the same)
                        let canonical_path =
                            StandardizedPath::from_local_canonicalized(&real_repo).unwrap();
                        assert!(model.repositories.contains_key(&canonical_path));
                    });

                    // Test find_repository_for_path with different path formats
                    model_handle.read(&app, |model, _ctx| {
                        let file_in_repo = real_repo.join("src/main.rs");
                        let symlink_file = symlink_repo.join("src/main.rs");

                        let found_real = model.find_repository_for_path(&file_in_repo);
                        let found_symlink = model.find_repository_for_path(&symlink_file);

                        // Both should find the same repository
                        assert!(found_real.is_some());
                        assert!(found_symlink.is_some());
                        assert_eq!(found_real, found_symlink);
                    });
                });
            }
        });
    }

    #[test]
    fn test_standardized_path_edge_cases() {
        use mnemos_util::standardized_path::StandardizedPath;

        VirtualFS::test("canonicalized_edge_cases", |dirs, mut vfs| {
            let test_root = dirs.tests();

            // Create test files and directories
            vfs.mkdir("existing_dir")
                .with_files(vec![Stub::FileWithContent("existing_file.txt", "content")]);

            let existing_dir = test_root.join("existing_dir");
            let existing_file = test_root.join("existing_file.txt");
            let nonexistent = test_root.join("nonexistent");

            // Test successful canonicalization
            assert!(StandardizedPath::from_local_canonicalized(&existing_dir).is_ok());
            assert!(StandardizedPath::from_local_canonicalized(&existing_file).is_ok());

            // Test failed canonicalization
            assert!(StandardizedPath::from_local_canonicalized(&nonexistent).is_err());

            // Test equality and hashing
            let canonical1 = StandardizedPath::from_local_canonicalized(&existing_dir).unwrap();
            let canonical2 = StandardizedPath::from_local_canonicalized(&existing_dir).unwrap();

            assert_eq!(canonical1, canonical2);

            // Test that they can be used in HashMaps
            let mut map = std::collections::HashMap::new();
            map.insert(canonical1.clone(), "value1");
            assert_eq!(map.get(&canonical2), Some(&"value1"));

            // Test Debug trait
            let debug_str = format!("{canonical1:?}");
            assert!(debug_str.contains("StandardizedPath"));
        });
    }

    /// A lazily-loaded tree state whose root is loaded and has no children.
    #[cfg(feature = "local_fs")]
    fn lazy_state_for(root: &StandardizedPath) -> FileTreeState {
        FileTreeState::new_lazy_loaded(Entry::Directory(DirectoryEntry {
            path: root.clone(),
            children: vec![],
            ignored: false,
            loaded: true,
        }))
    }

    #[cfg(feature = "local_fs")]
    #[test]
    fn test_find_repository_for_std_path_is_component_aware() {
        let path = |s: &str| StandardizedPath::try_new(s).unwrap();
        let root = path("/work");
        let repo = path("/work/repo");
        let pending = path("/work/pending");

        let mut model = LocalRepoMetadataModel::new_for_test();
        model.repositories.insert(
            repo.clone(),
            IndexedRepoState::Indexed(lazy_state_for(&repo)),
        );
        model
            .repositories
            .insert(pending.clone(), IndexedRepoState::Pending);

        assert_eq!(
            model.find_repository_for_std_path(&path("/work/repo/src/a.rs")),
            Some(repo.clone())
        );
        assert_eq!(
            model.find_repository_for_std_path(&path("/work/repo")),
            Some(repo.clone())
        );
        // A sibling that merely shares a string prefix is not inside the repo.
        assert_eq!(
            model.find_repository_for_std_path(&path("/work/repo-sibling/a.rs")),
            None
        );
        assert_eq!(
            model.find_repository_for_std_path(&path("/work/repository/a.rs")),
            None
        );
        // Pending repos never receive events.
        assert_eq!(
            model.find_repository_for_std_path(&path("/work/pending/a.rs")),
            None
        );

        // With an enclosing root (like a lazy root at $HOME), siblings go to the
        // enclosing root and the innermost repo still wins for its own paths.
        model.repositories.insert(
            root.clone(),
            IndexedRepoState::Indexed(lazy_state_for(&root)),
        );
        assert_eq!(
            model.find_repository_for_std_path(&path("/work/repo-sibling/a.rs")),
            Some(root.clone())
        );
        assert_eq!(
            model.find_repository_for_std_path(&path("/work/repo/src/a.rs")),
            Some(repo)
        );
        assert_eq!(
            model.find_repository_for_std_path(&path("/work/pending/a.rs")),
            Some(root)
        );
    }

    #[cfg(feature = "local_fs")]
    #[test]
    fn test_find_repository_for_path_ignores_sibling_prefix() {
        VirtualFS::test("repo_routing_sibling", |dirs, mut vfs| {
            vfs.mkdir("repo/src").mkdir("repo-sibling").with_files(vec![
                Stub::FileWithContent("repo/src/a.rs", "fn a() {}"),
                Stub::FileWithContent("repo-sibling/a.rs", "fn a() {}"),
            ]);

            let repo_dir = dirs.tests().join("repo");
            let repo_file = repo_dir.join("src/a.rs");
            let sibling_file = dirs.tests().join("repo-sibling/a.rs");
            // Both files exist, so a None below is due to routing, not canonicalization.
            assert!(repo_file.exists());
            assert!(sibling_file.exists());

            let repo = StandardizedPath::from_local_canonicalized(&repo_dir).unwrap();
            let mut model = LocalRepoMetadataModel::new_for_test();
            model.repositories.insert(
                repo.clone(),
                IndexedRepoState::Indexed(lazy_state_for(&repo)),
            );

            assert_eq!(model.find_repository_for_path(&repo_file), Some(repo));
            assert_eq!(model.find_repository_for_path(&sibling_file), None);
        });
    }

    #[cfg(feature = "local_fs")]
    fn std_path_of(path: &std::path::Path) -> StandardizedPath {
        StandardizedPath::try_from_local(path).unwrap()
    }

    #[cfg(feature = "local_fs")]
    fn dir_entry(path: &std::path::Path, loaded: bool, children: Vec<Entry>) -> Entry {
        Entry::Directory(DirectoryEntry {
            path: std_path_of(path),
            children,
            ignored: false,
            loaded,
        })
    }

    /// An absolute root for tests that only look at the in-memory tree.
    #[cfg(feature = "local_fs")]
    fn synthetic_root(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join("repo_metadata_lazy_filter")
            .join(name)
    }

    #[cfg(feature = "local_fs")]
    #[test]
    fn test_filter_update_for_lazy_root_drops_changes_in_unloaded_dirs() {
        let root = synthetic_root("unloaded");
        let a = root.join("a");
        let entry = FileTreeEntry::from(dir_entry(&root, true, vec![dir_entry(&a, false, vec![])]));

        let update = RepoUpdate {
            added: vec![a.join("new/deep/x"), a.join("new/deep"), a.join("new")],
            deleted: vec![a.join("gone")],
            moved: HashMap::from([(a.join("to"), a.join("from"))]),
        };
        let filtered = LocalRepoMetadataModel::filter_update_for_lazy_root(&entry, &update);
        assert!(filtered.is_empty(), "nothing should survive: {filtered:?}");
    }

    #[cfg(feature = "local_fs")]
    #[test]
    fn test_filter_update_for_lazy_root_keeps_changes_in_loaded_dirs() {
        let root = synthetic_root("loaded");
        let a = root.join("a"); // unloaded
        let b = root.join("b"); // loaded
        let file = root.join("file.txt");
        let b_old = b.join("old.rs");
        let entry = FileTreeEntry::from(dir_entry(
            &root,
            true,
            vec![
                dir_entry(&a, false, vec![]),
                dir_entry(
                    &b,
                    true,
                    vec![Entry::File(FileMetadata::new(b_old.clone(), false))],
                ),
                Entry::File(FileMetadata::new(file.clone(), false)),
            ],
        ));

        let update = RepoUpdate {
            added: vec![
                root.join("new_dir"),
                // Already-present directories (e.g. Windows Modify(Any) on a
                // parent) are dropped, whether loaded or not.
                a.clone(),
                b.clone(),
                // Existing files and new children of a loaded dir are kept.
                file.clone(),
                b.join("child.rs"),
                // A child of the unloaded a/ is dropped.
                a.join("child.rs"),
            ],
            deleted: vec![],
            moved: HashMap::from([
                // Moved into a loaded dir: kept as a move.
                (root.join("moved_in"), a.join("hidden")),
                // Moved into an unloaded dir: only the removal is kept.
                (a.join("moved_away"), b_old.clone()),
            ]),
        };
        let filtered = LocalRepoMetadataModel::filter_update_for_lazy_root(&entry, &update);

        assert_eq!(
            filtered.added,
            vec![root.join("new_dir"), file, b.join("child.rs")]
        );
        assert_eq!(
            filtered.moved,
            HashMap::from([(root.join("moved_in"), a.join("hidden"))])
        );
        assert_eq!(filtered.deleted, vec![b_old]);
    }

    #[cfg(feature = "local_fs")]
    #[test]
    fn test_filter_update_for_lazy_root_drops_unknown_deletes() {
        let root = synthetic_root("deletes");
        let a = root.join("a"); // unloaded
        let entry = FileTreeEntry::from(dir_entry(&root, true, vec![dir_entry(&a, false, vec![])]));

        // Paths under an absent or unloaded parent were never in the tree.
        let unknown = RepoUpdate {
            deleted: vec![root.join("missing/x"), a.join("gone")],
            ..Default::default()
        };
        assert!(LocalRepoMetadataModel::filter_update_for_lazy_root(&entry, &unknown).is_empty());

        // Paths in the tree, or directly under a loaded dir (an add from an
        // earlier batch may not be applied yet), are kept.
        let known = RepoUpdate {
            deleted: vec![a.clone(), root.join("pending.txt")],
            ..Default::default()
        };
        let filtered = LocalRepoMetadataModel::filter_update_for_lazy_root(&entry, &known);
        assert_eq!(filtered.deleted, vec![a, root.join("pending.txt")]);
        assert!(filtered.added.is_empty());
        assert!(filtered.moved.is_empty());
    }

    #[cfg(feature = "local_fs")]
    #[test]
    fn test_lazy_root_new_dir_is_added_unloaded_without_walking_it() {
        VirtualFS::test("lazy_root_new_dir", |dirs, mut vfs| {
            vfs.mkdir("root/new_dir/sub")
                .with_files(vec![Stub::FileWithContent(
                    "root/new_dir/sub/file.rs",
                    "fn f() {}",
                )]);
            let root = dirs.tests().join("root");
            let new_dir = root.join("new_dir");
            let mut entry = FileTreeEntry::from(dir_entry(&root, true, vec![]));

            let update = RepoUpdate {
                added: vec![new_dir.clone()],
                ..Default::default()
            };
            let filtered = LocalRepoMetadataModel::filter_update_for_lazy_root(&entry, &update);
            assert_eq!(filtered.added, vec![new_dir.clone()]);

            // Indexed repos still get the full subtree.
            let full = block_on(LocalRepoMetadataModel::compute_file_tree_mutations(
                &filtered,
                &[],
                false,
            ));
            assert!(matches!(
                full.as_slice(),
                [crate::local_model::FileTreeMutation::AddDirectorySubtree { .. }]
            ));

            let mutations = block_on(LocalRepoMetadataModel::compute_file_tree_mutations(
                &filtered,
                &[],
                true,
            ));
            assert_eq!(mutations.len(), 1, "{mutations:?}");
            assert!(matches!(
                &mutations[0],
                crate::local_model::FileTreeMutation::AddEmptyDirectory { path, .. }
                    if *path == new_dir
            ));

            LocalRepoMetadataModel::apply_file_tree_mutations(&mut entry, mutations, true, false);
            assert!(matches!(
                entry.get(&std_path_of(&new_dir)),
                Some(FileTreeEntryState::Directory(dir)) if !dir.loaded
            ));
            assert!(!entry.contains(&std_path_of(&new_dir.join("sub"))));
        });
    }

    #[cfg(feature = "local_fs")]
    #[test]
    fn test_lazy_root_dir_deleted_and_recreated_in_one_batch_stays() {
        VirtualFS::test("lazy_root_recreated_dir", |dirs, mut vfs| {
            vfs.mkdir("root/d")
                .with_files(vec![Stub::FileWithContent("root/d/new.rs", "fn f() {}")]);
            let root = dirs.tests().join("root");
            let d = root.join("d");
            let old_file = d.join("old.rs");
            // d/ is expanded in the tree and still lists a file from before.
            let mut entry = FileTreeEntry::from(dir_entry(
                &root,
                true,
                vec![dir_entry(
                    &d,
                    true,
                    vec![Entry::File(FileMetadata::new(old_file.clone(), false))],
                )],
            ));

            // `rm -rf d && mkdir d` in one debounce window lands in both lists.
            let update = RepoUpdate {
                added: vec![d.clone()],
                deleted: vec![d.clone()],
                moved: HashMap::new(),
            };
            let filtered = LocalRepoMetadataModel::filter_update_for_lazy_root(&entry, &update);
            assert_eq!(filtered.added, vec![d.clone()]);
            assert_eq!(filtered.deleted, vec![d.clone()]);

            let mutations = block_on(LocalRepoMetadataModel::compute_file_tree_mutations(
                &filtered,
                &[],
                true,
            ));
            LocalRepoMetadataModel::apply_file_tree_mutations(&mut entry, mutations, true, false);

            assert!(matches!(
                entry.get(&std_path_of(&d)),
                Some(FileTreeEntryState::Directory(dir)) if !dir.loaded
            ));
            assert!(!entry.contains(&std_path_of(&old_file)));
        });
    }

    #[cfg(feature = "local_fs")]
    #[test]
    fn test_handle_watcher_event_skips_changes_in_unloaded_lazy_dirs() {
        use ::watcher::BulkFilesystemWatcherEvent;
        use std::collections::HashSet;

        VirtualFS::test("lazy_root_watcher_event", |dirs, mut vfs| {
            vfs.mkdir("root/a/new/deep")
                .mkdir("root/new_dir/sub")
                .with_files(vec![
                    Stub::FileWithContent("root/a/new/deep/x.rs", "x"),
                    Stub::FileWithContent("root/new_dir/sub/y.rs", "y"),
                ]);
            let root = dirs.tests().join("root");
            let a = root.join("a");
            let new_dir = root.join("new_dir");
            let root_std = StandardizedPath::from_local_canonicalized(&root).unwrap();

            App::test((), |mut app| async move {
                let model_handle = app.add_model(|_| LocalRepoMetadataModel::new_for_test());
                model_handle.update(&mut app, |model, _ctx| {
                    let state = FileTreeState::new_lazy_loaded(dir_entry(
                        &root,
                        true,
                        vec![dir_entry(&a, false, vec![])],
                    ));
                    model
                        .repositories
                        .insert(root_std.clone(), IndexedRepoState::Indexed(state));
                    model.lazy_loaded_paths.insert(root_std.clone(), 1);
                });

                let tree_updates = Rc::new(RefCell::new(0usize));
                let (tx, rx) = oneshot::channel();
                let entry_updated = Rc::new(RefCell::new(Some(tx)));
                let tree_updates_for_event = tree_updates.clone();
                app.update(|ctx| {
                    ctx.subscribe_to_model(&model_handle, move |_, event, _ctx| match event {
                        RepositoryMetadataEvent::FileTreeUpdated { .. } => {
                            *tree_updates_for_event.borrow_mut() += 1;
                        }
                        RepositoryMetadataEvent::FileTreeEntryUpdated { .. } => {
                            if let Some(tx) = entry_updated.borrow_mut().take() {
                                let _ = tx.send(());
                            }
                        }
                        _ => {}
                    });
                });

                // Everything under the unloaded a/ is dropped before any work.
                let hidden = BulkFilesystemWatcherEvent {
                    added: HashSet::from([
                        a.join("new"),
                        a.join("new/deep"),
                        a.join("new/deep/x.rs"),
                    ]),
                    ..Default::default()
                };
                model_handle.update(&mut app, |model, ctx| {
                    model.handle_watcher_event(&hidden, ctx);
                });
                assert_eq!(*tree_updates.borrow(), 0);

                // A new directory directly under the loaded root is added unloaded.
                let visible = BulkFilesystemWatcherEvent {
                    added: HashSet::from([new_dir.clone()]),
                    ..Default::default()
                };
                model_handle.update(&mut app, |model, ctx| {
                    model.handle_watcher_event(&visible, ctx);
                });
                assert_eq!(*tree_updates.borrow(), 1);
                rx.with_timeout(Duration::from_secs(5))
                    .await
                    .expect("timed out waiting for the file tree update")
                    .expect("file tree update sender dropped");

                model_handle.read(&app, |model, _ctx| {
                    let Some(IndexedRepoState::Indexed(state)) = model.repository_state(&root_std)
                    else {
                        panic!("expected the lazy root to stay indexed");
                    };
                    assert!(matches!(
                        state.entry.get(&std_path_of(&new_dir)),
                        Some(FileTreeEntryState::Directory(dir)) if !dir.loaded
                    ));
                    assert!(!state.entry.contains(&std_path_of(&new_dir.join("sub"))));
                    assert!(!state.entry.contains(&std_path_of(&a.join("new"))));
                });
            });
        });
    }

    #[cfg(all(unix, feature = "local_fs"))]
    #[test]
    fn test_added_symlink_to_dir_is_skipped() {
        VirtualFS::test("watcher_symlinked_dir", |dirs, mut vfs| {
            vfs.mkdir("root/real/nested")
                .with_files(vec![Stub::FileWithContent("root/real/file.txt", "content")])
                .ln("root/real", "root/dir_link")
                .ln("root/real/file.txt", "root/file_link");
            let root = dirs.tests().join("root");
            let dir_link = root.join("dir_link");
            let file_link = root.join("file_link");
            assert!(dir_link.is_symlink() && dir_link.is_dir());

            let update = RepoUpdate {
                added: vec![dir_link.clone(), file_link.clone()],
                ..Default::default()
            };
            for lazy_load in [false, true] {
                let mut entry = FileTreeEntry::from(dir_entry(&root, true, vec![]));
                let mutations = block_on(LocalRepoMetadataModel::compute_file_tree_mutations(
                    &update,
                    &[],
                    lazy_load,
                ));
                // Only the symlink to a file produces a mutation.
                assert_eq!(mutations.len(), 1, "lazy_load={lazy_load}: {mutations:?}");
                assert!(matches!(
                    &mutations[0],
                    crate::local_model::FileTreeMutation::AddFile { path, .. } if *path == file_link
                ));

                LocalRepoMetadataModel::apply_file_tree_mutations(
                    &mut entry, mutations, lazy_load, false,
                );
                assert!(!entry.contains(&std_path_of(&dir_link)));
                assert!(entry.contains(&std_path_of(&file_link)));
            }
        });
    }

    #[cfg(all(unix, feature = "local_fs"))]
    #[test]
    fn test_unreadable_added_dir_still_falls_back_to_empty_dir() {
        use std::os::unix::fs::PermissionsExt;

        VirtualFS::test("watcher_unreadable_dir", |dirs, mut vfs| {
            vfs.mkdir("root/locked");
            let root = dirs.tests().join("root");
            let locked = root.join("locked");
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            // Root can read anything, so there is nothing to check there.
            let readable = std::fs::read_dir(&locked).is_ok();

            let update = RepoUpdate {
                added: vec![locked.clone()],
                ..Default::default()
            };
            let mutations = block_on(LocalRepoMetadataModel::compute_file_tree_mutations(
                &update,
                &[],
                false,
            ));
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

            if !readable {
                assert!(matches!(
                    mutations.as_slice(),
                    [crate::local_model::FileTreeMutation::AddEmptyDirectory { path, .. }]
                        if *path == locked
                ));
            }
        });
    }
}
