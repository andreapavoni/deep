use deep::proxy::CaddyFile;
use tempfile::TempDir;

#[test]
fn list_routes_empty_when_file_missing() {
    let temp = TempDir::new().expect("temp");
    let proxy = CaddyFile::new(temp.path().join("Caddyfile"), "deep-caddy".to_string());
    let routes = proxy.list_routes().expect("routes");
    assert!(routes.is_empty());
}
