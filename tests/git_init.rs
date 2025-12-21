use deep::cli::git::init_repo_for_app;
use deep::db::Storage;
use tempfile::TempDir;

#[test]
fn git_init_writes_post_receive_hook() {
    let temp = TempDir::new().expect("temp dir");
    let db_path = temp.path().join("deep.db");
    let mut storage = Storage::open(&db_path).expect("open db");
    let app = storage
        .create_app("myapp", "/srv/deep/repos/myapp.git")
        .expect("create app");

    let repos_dir = temp.path().join("repos");
    let repo_path = repos_dir.join("myapp.git");
    let repo_path = init_repo_for_app(
        &mut storage,
        &app.name,
        repos_dir.clone(),
        Some(repo_path.clone()),
        Some("local/{{app}}:{{sha}}".to_string()),
        "Dockerfile",
        "deep",
    )
    .expect("git init");

    let hook_path = repo_path.join("hooks").join("post-receive");
    let hook = std::fs::read_to_string(&hook_path).expect("read hook");
    assert!(hook.contains("podman build"));
    assert!(hook.contains("deep deploy"));
    assert!(hook.contains("--skip-pull"));
    assert!(hook.contains("local/{{app}}:{{sha}}"));
}
