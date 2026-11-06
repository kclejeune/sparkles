//! The process-wide cache budget is applied before serve starts.

#![cfg(feature = "reasoning")]

use serde_json::Value as J;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

struct Server {
    child: Child,
    base: String,
    client: reqwest::blocking::Client,
    _dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn start(extra: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let port = (48240..48260)
            .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .expect("a free test port");
        let child = Command::new(BIN)
            .args([
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--spill",
                "off",
                "--total-query-memory-mb",
                "64",
                "--result-cache-mb",
                "0",
            ])
            .arg("--data")
            .arg(dir.path().join("data"))
            .args(extra)
            .env_remove("SPARKLES_AUTH_CONFIG")
            .env_remove("SPARKLES_BACKUP_CONFIG")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.path().join("server.log")).unwrap())
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            base: format!("http://127.0.0.1:{port}"),
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap(),
            _dir: dir,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "server exited before readiness"
            );
            if server
                .client
                .get(format!("{}/$/ready", server.base))
                .send()
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            assert!(Instant::now() < deadline, "server did not become ready");
            std::thread::sleep(Duration::from_millis(25));
        }
        server
    }

    fn json(&self, uri: &str) -> J {
        serde_json::from_str(
            &self
                .client
                .get(format!("{}{uri}", self.base))
                .send()
                .unwrap()
                .error_for_status()
                .unwrap()
                .text()
                .unwrap(),
        )
        .unwrap()
    }

    fn demand_twice(&self) {
        self.client
            .post(format!("{}/$/datasets?dbName=ds&dbType=mem", self.base))
            .send()
            .unwrap()
            .error_for_status()
            .unwrap();
        self.client.post(format!("{}/ds/update", self.base))
            .header("content-type", "application/sparql-update")
            .body("INSERT DATA { <urn:child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:parent> . <urn:s> a <urn:child> }")
            .send().unwrap().error_for_status().unwrap();
        for _ in 0..2 {
            let response = self
                .client
                .post(format!(
                    "{}/ds/sparql?reasoning=demand&reasoningProfile=rdfs",
                    self.base
                ))
                .header("content-type", "application/sparql-query")
                .header("accept", "application/sparql-results+json")
                .body("ASK { <urn:s> a <urn:parent> }")
                .send()
                .unwrap()
                .error_for_status()
                .unwrap();
            let answer: J = serde_json::from_str(&response.text().unwrap()).unwrap();
            assert_eq!(answer["boolean"], true);
        }
    }
}

#[test]
fn serve_applies_the_cache_budget_and_reports_retained_usage() {
    for (args, capacity) in [
        (Vec::new(), 256u64 << 20),
        (vec!["--derived-table-cache-mb", "0"], 0),
        (
            vec!["--memory-profile", "small", "--derived-table-cache-mb", "1"],
            1 << 20,
        ),
    ] {
        let server = Server::start(&args);
        server.demand_twice();
        let all = server.json("/$/stats");
        let cache = &all["derivedTableCache"];
        assert_eq!(cache["scope"], "process");
        assert_eq!(cache["capacityBytes"], capacity);
        assert_eq!(cache["enabled"], capacity > 0);
        if capacity == 0 {
            assert_eq!(cache["estimatedBytes"], 0);
            assert_eq!(cache["tables"], 0);
            assert_eq!(cache["hits"], 0);
        } else {
            assert!(cache["estimatedBytes"].as_u64().unwrap() > 0);
            assert!(cache["estimatedBytes"].as_u64().unwrap() <= capacity);
            assert!(cache["tables"].as_u64().unwrap() > 0);
            assert!(cache["hits"].as_u64().unwrap() > 0);
        }
        assert_eq!(&server.json("/$/stats/ds")["derivedTableCache"], cache);
        let memory = server.json("/$/server/memory");
        assert_eq!(&memory["caches"]["derivedTable"], cache);
        assert_eq!(memory["pool"]["capacityBytes"], 64u64 << 20);
    }
}
