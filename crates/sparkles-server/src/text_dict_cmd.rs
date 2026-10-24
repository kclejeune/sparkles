//! `sparkles text-dict`: the dictionaries of the `ja-dict`, `zh-dict` and `ko-dict`
//! full-text analyzers (spec F03, CJK dictionary segmentation). `fetch` downloads a
//! pinned source with a verified SHA-256 and compiles it, `install` compiles a local
//! source, and `list` reads the installed dictionaries. Each dictionary is
//! `DIR/<name>.sdict`, with its source's license beside it as `DIR/<name>.license.txt`
//! and a description of where it came from as `DIR/<name>.json`.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::json;
use sha2::Digest;
use sparkles::text::dict;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::outbound::OutboundArgs;

#[derive(Args, Debug)]
pub struct TextDictArgs {
    #[command(subcommand)]
    cmd: TextDictCmd,
}

/// Where the dictionaries are: `--dir`, or `<--data>/text-dicts`, or
/// `$SPARKLES_TEXT_DICTS`, or `./data/text-dicts` (the default of `serve --data`).
#[derive(Args, Debug, Clone)]
pub struct DictDirArgs {
    /// The directory of installed dictionaries
    #[arg(long, value_name = "DIR", conflicts_with = "data")]
    dir: Option<PathBuf>,
    /// A server's data directory; the dictionaries are DIR/text-dicts
    #[arg(long, value_name = "DIR")]
    data: Option<PathBuf>,
}

impl DictDirArgs {
    pub fn apply(&self) {
        match (&self.dir, &self.data) {
            (Some(d), _) => dict::set_dir(d),
            (None, Some(d)) => dict::set_dir(d.join("text-dicts")),
            (None, None) => {}
        }
    }
}

#[derive(Subcommand, Debug)]
enum TextDictCmd {
    /// Download a dictionary's pinned source, check its SHA-256, compile and install it
    Fetch {
        /// ja-dict (IPADIC), zh-dict (jieba's word list) or ko-dict (mecab-ko-dic)
        name: String,
        #[command(flatten)]
        dir: DictDirArgs,
        #[command(flatten)]
        outbound: OutboundArgs,
    },
    /// Compile and install a dictionary from a local source: a MeCab dictionary
    /// directory for ja-dict (IPADIC's layout) and ko-dict (mecab-ko-dic's), or a word
    /// list of `word frequency [tag]` lines for zh-dict
    Install {
        name: String,
        /// The source directory or file
        #[arg(long, value_name = "PATH")]
        from: PathBuf,
        /// The source's license, copied beside the dictionary
        #[arg(long, value_name = "FILE")]
        license: Option<PathBuf>,
        #[command(flatten)]
        dir: DictDirArgs,
    },
    /// List the installed dictionaries, reading and checking each
    List {
        #[command(flatten)]
        dir: DictDirArgs,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
}

/// A pinned dictionary source.
struct Pinned {
    name: &'static str,
    url: &'static str,
    sha256: &'static str,
    /// a gzipped tar archive of a MeCab dictionary, or a plain word list
    archive: bool,
    /// the license: a file of the archive, or a separate pinned file (URL, SHA-256)
    license_member: Option<&'static str>,
    license_url: Option<(&'static str, &'static str)>,
    license: &'static str,
}

const PINNED: [Pinned; 3] = [
    Pinned {
        name: "ja-dict",
        url: "https://deb.debian.org/debian/pool/main/m/mecab-ipadic/mecab-ipadic_2.7.0-20070801+main.orig.tar.gz",
        sha256: "b62f527d881c504576baed9c6ef6561554658b175ce6ae0096a60307e49e3523",
        archive: true,
        license_member: Some("COPYING"),
        license_url: None,
        license: "IPADIC 2.7.0 (NAIST and ICOT terms, permissive with notice)",
    },
    Pinned {
        name: "ko-dict",
        url: "https://bitbucket.org/eunjeon/mecab-ko-dic/downloads/mecab-ko-dic-2.1.1-20180720.tar.gz",
        sha256: "fd62d3d6d8fa85145528065fabad4d7cb20f6b2201e71be4081a4e9701a5b330",
        archive: true,
        license_member: Some("COPYING"),
        license_url: None,
        license: "mecab-ko-dic 2.1.1 (Apache-2.0)",
    },
    Pinned {
        name: "zh-dict",
        url: "https://raw.githubusercontent.com/fxsjy/jieba/67fa2e36e72f69d9134b8a1037b83fbb070b9775/jieba/dict.txt",
        sha256: "7197c3211ddd98962b036cdf40324d1ea2bfaa12bd028e68faa70111a88e12a8",
        archive: false,
        license_member: None,
        license_url: Some((
            "https://raw.githubusercontent.com/fxsjy/jieba/67fa2e36e72f69d9134b8a1037b83fbb070b9775/LICENSE",
            "18ba0984839f85853b29fadaf992f7dba8fd0ca0fbeae34de2b8735222dc7a37",
        )),
        license: "jieba 0.42.1 word list (MIT)",
    },
];

fn layout_of(name: &str) -> Result<dict::Layout> {
    dict::Layout::of(name).with_context(|| {
        format!("{name:?} is not a dictionary analyzer (ja-dict, zh-dict or ko-dict)")
    })
}

pub fn run(args: TextDictArgs) -> Result<()> {
    match args.cmd {
        TextDictCmd::Fetch {
            name,
            dir,
            outbound,
        } => {
            dir.apply();
            layout_of(&name)?;
            let p = PINNED
                .iter()
                .find(|p| p.name == name)
                .expect("every analyzer is pinned");
            let policy = crate::model_store::download_policy(&outbound.local_policy()?);
            eprintln!("downloading {}", p.url);
            let source = download(&policy, p.url, p.sha256)?;
            let work = tempfile::tempdir()?;
            let (from, license) = if p.archive {
                let license = extract(&source, work.path(), p.license_member)?;
                (work.path().to_path_buf(), license)
            } else {
                let f = work.path().join("source.txt");
                std::fs::write(&f, &source)?;
                let license = match p.license_url {
                    Some((url, sha)) => Some(download(&policy, url, sha)?),
                    None => None,
                };
                (f, license)
            };
            let meta = json!({
                "source": p.url,
                "sourceSha256": p.sha256,
                "license": p.license,
            });
            install(&name, &from, license.as_deref(), meta)
        }
        TextDictCmd::Install {
            name,
            from,
            license,
            dir,
        } => {
            dir.apply();
            layout_of(&name)?;
            let license = match &license {
                Some(f) => {
                    Some(std::fs::read(f).with_context(|| format!("cannot read {}", f.display()))?)
                }
                None => None,
            };
            let meta = json!({ "source": from.display().to_string() });
            install(&name, &from, license.as_deref(), meta)
        }
        TextDictCmd::List { dir, json } => {
            dir.apply();
            let mut rows = Vec::new();
            for name in ["ja-dict", "zh-dict", "ko-dict"] {
                let path = dict::path_of(name);
                if !path.exists() {
                    continue;
                }
                let meta: serde_json::Value =
                    std::fs::read(dict::dir().join(format!("{name}.json")))
                        .ok()
                        .and_then(|b| serde_json::from_slice(&b).ok())
                        .unwrap_or(serde_json::Value::Null);
                let row = match dict::load(name) {
                    Ok(d) => json!({
                        "name": name,
                        "path": path.display().to_string(),
                        "words": d.words(),
                        "sha256": d.sha256,
                        "source": meta.get("source"),
                        "license": meta.get("license"),
                    }),
                    Err(e) => json!({
                        "name": name,
                        "path": path.display().to_string(),
                        "error": e.to_string(),
                    }),
                };
                rows.push(row);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else if rows.is_empty() {
                println!("no dictionaries in {}", dict::dir().display());
            } else {
                for r in &rows {
                    match r.get("error") {
                        Some(e) => println!("{} {} damaged: {}", r["name"], r["path"], e),
                        None => println!(
                            "{} {} words, sha256 {} ({})",
                            r["name"].as_str().unwrap_or(""),
                            r["words"],
                            r["sha256"].as_str().unwrap_or(""),
                            r["source"].as_str().unwrap_or("unknown source")
                        ),
                    }
                }
            }
            if rows.iter().any(|r| r.get("error").is_some()) {
                bail!("a dictionary could not be read");
            }
            Ok(())
        }
    }
}

/// Compiles `from` and installs it as `name`, with its license and description.
fn install(
    name: &str,
    from: &Path,
    license: Option<&[u8]>,
    mut meta: serde_json::Value,
) -> Result<()> {
    let t = std::time::Instant::now();
    let bytes = dict::compile(from, layout_of(name)?)?;
    let sha = dict::install(name, &bytes)?;
    let d = dict::load(name)?;
    let dir = dict::dir();
    if let Some(l) = license {
        std::fs::write(dir.join(format!("{name}.license.txt")), l)?;
    }
    meta["sha256"] = json!(sha);
    meta["words"] = json!(d.words());
    meta["formatVersion"] = json!(dict::FORMAT_VERSION);
    std::fs::write(
        dir.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&meta)?,
    )?;
    eprintln!(
        "installed {name}: {} words, {:.1} MiB, sha256 {sha}, in {:.1} s ({})",
        d.words(),
        bytes.len() as f64 / (1 << 20) as f64,
        t.elapsed().as_secs_f64(),
        dict::path_of(name).display()
    );
    eprintln!(
        "a full-text index that uses {name} is built again with this dictionary when its store next opens"
    );
    Ok(())
}

/// Downloads `url` within the outbound policy and checks its SHA-256.
fn download(
    policy: &sparkles::outbound::OutboundPolicy,
    url: &str,
    sha256: &str,
) -> Result<Vec<u8>> {
    let (status, mut body) = sparkles::outbound::get_stream(policy, url, &[], policy.timeout)
        .map_err(|f| anyhow::anyhow!("{url}: {f}"))?;
    if status != 200 {
        bail!("{url}: HTTP {status}");
    }
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes)
        .with_context(|| format!("reading {url}"))?;
    let got: String = sha2::Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got != sha256 {
        bail!("{url}: SHA-256 {got}, expected {sha256}; nothing was installed");
    }
    Ok(bytes)
}

/// Extracts the files a MeCab dictionary needs (`*.csv`, `matrix.def`, `char.def` and
/// `unk.def`) from a gzipped tar archive into `to`, and returns the license member.
fn extract(archive: &[u8], to: &Path, license: Option<&str>) -> Result<Option<Vec<u8>>> {
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(archive)
        .read_to_end(&mut tar)
        .context("the archive is not gzip")?;
    let mut found_license = None;
    let mut long_name: Option<String> = None;
    let mut at = 0;
    while at + 512 <= tar.len() {
        let h = &tar[at..at + 512];
        if h.iter().all(|&b| b == 0) {
            break;
        }
        let field = |r: std::ops::Range<usize>| {
            let f = &h[r];
            let end = f.iter().position(|&b| b == 0).unwrap_or(f.len());
            String::from_utf8_lossy(&f[..end]).into_owned()
        };
        let size = usize::from_str_radix(field(124..136).trim(), 8)
            .context("a tar header has a bad size")?;
        let kind = h[156];
        let mut name = field(0..100);
        let prefix = field(345..500);
        if !prefix.is_empty() && &h[257..262] == b"ustar" {
            name = format!("{prefix}/{name}");
        }
        if let Some(l) = long_name.take() {
            name = l;
        }
        let data_at = at + 512;
        let end = data_at
            .checked_add(size)
            .filter(|&e| e <= tar.len())
            .context("the archive is truncated")?;
        let data = &tar[data_at..end];
        match kind {
            b'L' => {
                long_name = Some(
                    String::from_utf8_lossy(data)
                        .trim_end_matches('\0')
                        .to_string(),
                )
            }
            b'0' | 0 => {
                // the file name alone: the archive's directories are not kept
                let base = name.rsplit('/').next().unwrap_or("");
                let wanted =
                    base.ends_with(".csv") || matches!(base, "matrix.def" | "char.def" | "unk.def");
                // only the archive's top directory: mecab-ko-dic keeps sample user
                // dictionaries in a subdirectory
                let top = name.trim_start_matches("./").matches('/').count() <= 1;
                if wanted && top && !base.is_empty() {
                    std::fs::write(to.join(base), data)?;
                }
                if license == Some(base) && top {
                    found_license = Some(data.to_vec());
                }
            }
            _ => {}
        }
        at = data_at + size.div_ceil(512) * 512;
    }
    Ok(found_license)
}
