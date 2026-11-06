//! A real one-shot child process removes its uploaded bytes and receipts on stop.
#![cfg(unix)]

use artifree_core::artifacts::ArtifactService;
use artifree_core::assets::AssetUpload;
use artifree_domain::assets::AssetVersion;
use artifree_domain::identity::UnixMillis;
use artifree_domain::package::FilePackage;
use artifree_providers::storage::SqliteStore;
use artifree_server::limits::RequestLimits;
use artifree_server::one_shot::{self, OneShotOptions};
use artifree_server::serve::{self, ServeOptions};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const CHILD_ROOT: &str = "ARTIFREE_ASSET_SESSION_TEST_ROOT";
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}
#[test]
fn a_stopped_one_shot_process_removes_uploads_and_receipts() -> TestResult {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        return run_child(PathBuf::from(root));
    }
    let root = std::env::temp_dir().join(format!("artifree-asset-session-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let mut child = Running(
        Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "a_stopped_one_shot_process_removes_uploads_and_receipts",
                "--nocapture",
            ])
            .env(CHILD_ROOT, &root)
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let result = loop {
        if let Some(status) = child.0.try_wait()? {
            break Ok(status);
        }
        if Instant::now() >= deadline {
            break Err("one-shot process did not stop");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    drop(child);
    let entries = std::fs::read_dir(&root)?.count();
    std::fs::remove_dir_all(&root)?;
    assert!(
        result?.success(),
        "the child failed its upload/disposal checks"
    );
    assert_eq!(
        entries, 0,
        "the session root must hold no database, WAL, marker or token"
    );
    Ok(())
}
fn run_child(root: PathBuf) -> TestResult {
    let options = OneShotOptions {
        serve: ServeOptions {
            data_dir: PathBuf::new(),
            web_dir: None,
            api_address: "127.0.0.1:0".parse()?,
            loopback_api_address: None,
            content_address: "127.0.0.1:0".parse()?,
            content_origin: None,
            frame_ancestors: serve::DEFAULT_FRAME_ANCESTORS.to_owned(),
            preview_ttl: serve::DEFAULT_PREVIEW_TTL,
            limits: RequestLimits::default(),
            change_streams: 4,
            api_origin: None,
            trusted_proxies: Vec::new(),
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
            allow_remote_mcp: false,
        },
        session_root: Some(root),
        ttl: Duration::from_secs(60),
    };
    let mut session = None;
    one_shot::run(Instant::now(), &options, |ready, _| {
        session = Some(ready.data_dir.clone());
        store_upload(&ready.data_dir).map_err(|error| error.to_string())?;
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(std::process::id().to_string())
            .status()
            .map_err(|error| error.to_string())?;
        if !status.success() {
            return Err("could not signal the session".into());
        }
        Ok(())
    })?;
    let path = session.ok_or("session never became ready")?;
    assert!(
        !path.exists(),
        "session directory survived: {}",
        path.display()
    );
    Ok(())
}
fn store_upload(path: &std::path::Path) -> TestResult {
    let service = ArtifactService::new(SqliteStore::open(&path.join("artifree.sqlite3"))?);
    let actor = "local".parse()?;
    let create_key = "one-shot-create".parse()?;
    let title = "Temporary upload".parse()?;
    let package = FilePackage::new(vec![(
        "index.html".parse()?,
        b"<h1>temporary</h1>".to_vec(),
    )])?;
    let created = service.create(&actor, &create_key, &title, &package)?;
    let key = "one-shot-upload".parse()?;
    let media = "text/plain".parse()?;
    let receipt = service.upload_asset(
        &actor,
        &key,
        &created.artifact.id,
        AssetUpload {
            base: AssetVersion(0),
            content_type: &media,
            bytes: b"one-shot private bytes",
            now: UnixMillis(1),
        },
    )?;
    assert_eq!(
        service
            .asset_content(&actor, &created.artifact.id, &receipt.id)?
            .bytes,
        b"one-shot private bytes"
    );
    let receipts: i64 = rusqlite::Connection::open(path.join("artifree.sqlite3"))?.query_row(
        "SELECT count(*) FROM asset_requests",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(receipts, 1);
    Ok(())
}
