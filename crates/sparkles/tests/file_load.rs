//! `LOAD <file:…>` under [`FileLoads`]: anywhere by default, only regular files inside
//! a directory with [`FileLoads::Under`] (no `..`, no symbolic link out of it), or not
//! at all.

use sparkles::Error;
use sparkles::sparql::{FileLoads, QueryOptions};
use sparkles::store::{Store, StoreOptions};
use std::path::Path;

const DATA: &str = "<urn:a> <urn:p> <urn:b> .\n";

fn file_url(p: &Path) -> String {
    format!("file://{}", p.display())
}

fn load(url: &str, files: &FileLoads) -> (sparkles::Result<()>, u64) {
    let store = Store::in_memory(StoreOptions::default());
    let opts = QueryOptions {
        file_loads: files.clone(),
        ..Default::default()
    };
    let r = sparkles::sparql::update::update(&store, &format!("LOAD <{url}>"), &opts);
    (r.map(|_| ()), store.snapshot().len())
}

fn refusal(r: sparkles::Result<()>) -> String {
    match r {
        Err(Error::NotPermitted(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn loads_stay_in_the_load_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("loads");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.nt"), DATA).unwrap();
    std::fs::write(dir.join("sub/b.nt"), DATA).unwrap();
    std::fs::write(dir.join("with space.nt"), DATA).unwrap();
    let outside = tmp.path().join("secret.nt");
    std::fs::write(&outside, DATA).unwrap();
    let under = FileLoads::under(&dir).unwrap();

    // files inside, at any depth, percent-encoded names included
    for url in [
        file_url(&dir.join("a.nt")),
        file_url(&dir.join("sub/b.nt")),
        format!("{}/with%20space.nt", file_url(&dir)),
        // dot segments that stay inside
        format!("{}/sub/../a.nt", file_url(&dir)),
    ] {
        let (r, n) = load(&url, &under);
        r.unwrap_or_else(|e| panic!("{url}: {e}"));
        assert_eq!(n, 1, "{url}");
    }

    // outside: refused, and the message does not tell whether the file exists
    let missing = tmp.path().join("missing.nt");
    for url in [
        file_url(&outside),
        file_url(&missing),
        format!("{}/../secret.nt", file_url(&dir)),
        format!("{}/sub/../../secret.nt", file_url(&dir)),
        "file:///etc/hostname".to_string(),
        "file://otherhost/etc/hostname".to_string(),
        "file:relative.nt".to_string(),
    ] {
        let (r, n) = load(&url, &under);
        let m = refusal(r);
        assert!(
            m.ends_with("not a file in the load directory"),
            "{url}: {m}"
        );
        assert_eq!(n, 0);
    }
    // SILENT does not hide the refusal
    let store = Store::in_memory(StoreOptions::default());
    let opts = QueryOptions {
        file_loads: under.clone(),
        ..Default::default()
    };
    let r = sparkles::sparql::update::update(
        &store,
        &format!("LOAD SILENT <{}>", file_url(&outside)),
        &opts,
    );
    assert!(matches!(r, Err(Error::NotPermitted(_))), "{r:?}");

    // a missing file inside is a plain failure
    let (r, _) = load(&file_url(&dir.join("nope.nt")), &under);
    assert!(
        matches!(r, Err(Error::Invalid(ref m)) if m.ends_with("no such file")),
        "{r:?}"
    );

    // not a directory, nor a file whose name does not end the path
    let (r, _) = load(&file_url(&dir.join("sub")), &under);
    refusal(r);
}

#[cfg(unix)]
#[test]
fn symbolic_links_and_special_files_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("loads");
    std::fs::create_dir_all(&dir).unwrap();
    let outside = tmp.path().join("secret.nt");
    std::fs::write(&outside, DATA).unwrap();
    std::fs::write(dir.join("a.nt"), DATA).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("link.nt")).unwrap();
    std::os::unix::fs::symlink(tmp.path(), dir.join("up")).unwrap();
    std::os::unix::fs::symlink(dir.join("a.nt"), dir.join("inside.nt")).unwrap();
    let under = FileLoads::under(&dir).unwrap();
    for url in [
        file_url(&dir.join("link.nt")),
        file_url(&dir.join("up/secret.nt")),
    ] {
        let (r, n) = load(&url, &under);
        refusal(r);
        assert_eq!(n, 0);
    }
    // a link that stays inside is followed
    let (r, n) = load(&file_url(&dir.join("inside.nt")), &under);
    r.unwrap();
    assert_eq!(n, 1);
    // a FIFO would block the reader
    let fifo = dir.join("fifo.nt");
    if std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|s| s.success())
    {
        let (r, _) = load(&file_url(&fifo), &under);
        refusal(r);
    }
    // the load directory may be named, or its files reached, through a link
    let alias = tmp.path().join("alias");
    std::os::unix::fs::symlink(&dir, &alias).unwrap();
    let (r, n) = load(
        &file_url(&dir.join("a.nt")),
        &FileLoads::under(&alias).unwrap(),
    );
    r.unwrap();
    assert_eq!(n, 1);
    let (r, n) = load(&file_url(&alias.join("a.nt")), &under);
    r.unwrap();
    assert_eq!(n, 1);
}

#[test]
fn file_loads_can_be_disabled_or_open() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.nt");
    std::fs::write(&f, DATA).unwrap();
    let m = refusal(load(&file_url(&f), &FileLoads::Disabled).0);
    assert_eq!(
        m,
        "LOAD <file:…> is not enabled: no load directory is configured"
    );
    // the default reads any file
    let (r, n) = load(&file_url(&f), &FileLoads::default());
    r.unwrap();
    assert_eq!(n, 1);
    // the permission is checked first
    let store = Store::in_memory(StoreOptions::default());
    let opts = QueryOptions {
        forbid_file_load: true,
        file_loads: FileLoads::Disabled,
        ..Default::default()
    };
    let r = sparkles::sparql::update::update(&store, &format!("LOAD <{}>", file_url(&f)), &opts);
    assert!(
        matches!(r, Err(Error::NotPermitted(ref m)) if m.contains("server-admin")),
        "{r:?}"
    );
    assert!(FileLoads::under(&tmp.path().join("missing")).is_err());
    assert!(FileLoads::under(&f).is_err());
}

/// A compressed file is decompressed as it is read.
#[test]
fn compressed_files_load() {
    use std::io::Write;
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.nt.gz");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(DATA.as_bytes()).unwrap();
    std::fs::write(&f, gz.finish().unwrap()).unwrap();
    let (r, n) = load(&file_url(&f), &FileLoads::under(tmp.path()).unwrap());
    r.unwrap();
    assert_eq!(n, 1);
}
