use super::*;
use std::io::{ErrorKind, Write};
use std::path::Path;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("sparkles-ack-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Writer {
    release: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<std::io::Result<()>>>,
}
impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(tx) = self.release.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.thread.take() {
            h.join().unwrap().unwrap();
        }
    }
}

fn visibility(name: &'static str, prefix: bool) {
    let dir = Directory::new();
    let path = dir.0.clone();
    let bytes = serde_json::to_vec(&serde_json::json!({"ack":name,"complete":[1,2,3]})).unwrap();
    let expected = bytes.clone();
    let (paused_tx, paused_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        publish_bytes(&path, name, &bytes, |file, bytes| {
            let n = if prefix { bytes.len() / 2 } else { 0 };
            file.write_all(&bytes[..n])?;
            paused_tx.send(n).unwrap();
            release_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("reader did not release writer");
            file.write_all(&bytes[n..])
        })
    });
    let writer = Writer {
        release: Some(release_tx),
        thread: Some(thread),
    };
    let n = paused_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        std::fs::read(dir.0.join(format!(".{name}.json.tmp"))).unwrap(),
        expected[..n]
    );
    assert_eq!(
        dir.0
            .join(format!("{name}.json"))
            .symlink_metadata()
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
    drop(writer); // Release and join even on a failed reader assertion.
    let actual = std::fs::read(dir.0.join(format!("{name}.json"))).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&actual).unwrap()["ack"],
        name
    );
    assert!(!dir.0.join(format!(".{name}.json.tmp")).exists());
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
}
#[test]
fn algo_stats_ack_entered_creation_visibility() {
    visibility("entered", false);
}
#[test]
fn algo_stats_ack_entered_prefix_visibility() {
    visibility("entered", true);
}
#[test]
fn algo_stats_ack_finished_creation_visibility() {
    visibility("finished", false);
}
#[test]
fn algo_stats_ack_finished_prefix_visibility() {
    visibility("finished", true);
}

#[test]
fn algo_stats_ack_stale_duplicate_and_failed_creation_preserve_owner() {
    for name in ["entered", "finished"] {
        let dir = Directory::new();
        let final_path = dir.0.join(format!("{name}.json"));
        let value = serde_json::json!({"original":true});
        publish_ack(&dir.0, name, &value).unwrap();
        let bytes = std::fs::read(&final_path).unwrap();
        assert_eq!(
            publish_ack(&dir.0, name, &serde_json::json!({"different":true}))
                .unwrap_err()
                .kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&final_path).unwrap(), bytes);
        std::fs::remove_file(&final_path).unwrap();
        std::os::unix::fs::symlink("missing-stale-target", &final_path).unwrap();
        assert_eq!(
            publish_ack(&dir.0, name, &value).unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read_link(&final_path).unwrap(),
            Path::new("missing-stale-target")
        );
        std::fs::remove_file(final_path).unwrap();
        let temp = dir.0.join(format!(".{name}.json.tmp"));
        std::fs::write(&temp, b"other publisher").unwrap();
        assert_eq!(
            publish_ack(&dir.0, name, &value).unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(temp).unwrap(), b"other publisher");
    }
}
#[test]
fn algo_stats_ack_failed_write_removes_only_owned_temp() {
    for name in ["entered", "finished"] {
        let dir = Directory::new();
        std::fs::write(dir.0.join("unrelated"), b"keep").unwrap();
        let error = publish_bytes(&dir.0, name, b"complete JSON", |file, bytes| {
            file.write_all(&bytes[..4])?;
            Err(std::io::Error::new(
                ErrorKind::PermissionDenied,
                "original write failure",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "original write failure");
        assert!(!dir.0.join(format!("{name}.json")).exists());
        assert!(!dir.0.join(format!(".{name}.json.tmp")).exists());
        assert_eq!(std::fs::read(dir.0.join("unrelated")).unwrap(), b"keep");
    }
}

#[test]
fn algo_stats_ack_failed_publication_child() {
    let Ok(mode) = std::env::var("SPARKLES_ACK_UNIT_CHILD") else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os("SPARKLES_TEST_ALGO_STATS_DIR").unwrap());
    let ds = sparkles::Dataset::memory();
    ds.update("INSERT DATA { <urn:a> <urn:p> <urn:b> }")
        .unwrap();
    let ctl = sparkles::task::Control::none();
    let request = serde_json::json!({"request":{"algorithm":"node2vec","projection":{"predicates":["urn:p"]},"params":{"dimension":1,"walkLength":70000,"walksPerNode":1,"negativeSamples":0,"epochs":1,"concurrency":1}}});
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        super::super::run_admin(
            &ds,
            "algo.stats",
            &request,
            &serde_json::json!({}),
            Arc::new(AtomicBool::new(false)),
            &ctl,
        )
    }));
    assert!(
        result.is_err(),
        "failed publication must panic at original observer boundary"
    );
    assert!(!dir.join("entered.json").exists());
    assert!(!dir.join("finished.json").exists());
    assert_eq!(
        std::fs::read(dir.join(".entered.json.tmp")).unwrap(),
        b"other publisher"
    );
    // Probe Drop must already have cleared its observer and released/joined any
    // slot holder. Actual admission and a second native call prove cleanup.
    let held = Par::acquire(
        1,
        &Control::new(
            Arc::new(AtomicBool::new(false)),
            Some(Instant::now() + Duration::from_secs(1)),
            u64::MAX,
        ),
    )
    .unwrap();
    drop(held);
    std::fs::remove_file(dir.join("armed")).unwrap();
    let result = super::super::run_admin(
        &ds,
        "algo.stats",
        &serde_json::json!({"request":{"algorithm":"wcc"}}),
        &serde_json::json!({}),
        Arc::new(AtomicBool::new(false)),
        &ctl,
    )
    .unwrap();
    assert_eq!(result["components"], 1);
    println!(
        "{}",
        serde_json::json!({"mode":mode,"publicationFailed":true,"holderJoinedAndReusable":true,"secondNativeComponents":1})
    );
}
#[test]
fn algo_stats_ack_failed_publication_releases_and_joins_holders() {
    for mode in ["compute", "slot"] {
        let dir = Directory::new();
        std::fs::write(dir.0.join("armed"), mode).unwrap();
        std::fs::write(dir.0.join(".entered.json.tmp"), b"other publisher").unwrap();
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                if self.0.try_wait().ok().flatten().is_none() {
                    let _ = self.0.kill();
                }
                let _ = self.0.wait();
            }
        }
        let mut child=Child(std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","admin::algo_stats_probe::publication_tests::algo_stats_ack_failed_publication_child","--nocapture"]).env("SPARKLES_ACK_UNIT_CHILD",mode).env("SPARKLES_TEST_ALGO_STATS_DIR",&dir.0).spawn().unwrap());
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success(), "{mode}: {status}");
                break;
            }
            assert!(Instant::now() < end, "failed publisher child timed out");
            std::thread::park_timeout(Duration::from_millis(5));
        }
    }
}
