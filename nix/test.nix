# NixOS VM test: the service starts, serves a declared dataset through nginx, keeps
# data across restarts, serves the embedded UI, and backs a dataset up into an `fs`
# repository of its backup config and restores it; with authentication and native TLS
# (node `authed`), it serves HTTP/1.1 and HTTP/2 over TLS, nginx proxies to it over https,
# and it tells clients behind nginx apart for the failed-login budget. The automatic
# compaction and clone options reach the server: a policy on one node, compaction turned
# off on the other.
{ self }:
{
  name = "sparkles";

  nodes.machine =
    { ... }:
    {
      imports = [ self.nixosModules.default ];
      services.sparkles = {
        enable = true;
        datasets = {
          demo = { };
          archive.path = "/srv/sparkles-archive";
          scratch.type = "mem";
        };
        datasetSettings = {
          demo = {
            assistant = {
              enabled = true;
              ingest = true;
              send = "documents";
            };
            memory = {
              agentGraphs = [ "urn:x-sparkles:import/*" ];
            };
          };
          archive.assistant = {
            enabled = true;
            send = "rows";
          };
          later.assistant = {
            enabled = true;
            ingest = true;
            send = "documents";
          };
        };
        models = {
          settings = {
            providers = {
              claude = {
                kind = "anthropic";
                endpoint = "https://api.anthropic.com";
                apiKey.secret = "anthropic";
              };
              gateway = {
                kind = "openai";
                endpoint = "https://api.openai.com/v1";
                apiKey.secret = "openai";
              };
            };
            roles = {
              draft = [
                {
                  provider = "claude";
                  model = "claude-haiku-5-5";
                }
              ];
              extract = [
                {
                  provider = "claude";
                  model = "claude-sonnet-5-5";
                  maxOutputTokens = 8192;
                }
              ];
            };
          };
          secrets = {
            anthropic.file = "/var/lib/sparkles/test-model-key";
            openai.environment = "TEST_OPENAI_API_KEY";
          };
        };
        nginx = {
          enable = true;
          virtualHost = "sparkles.test";
        };
        backup = {
          configFile = "/etc/sparkles/backup.toml";
          maxTasks = 1;
          fsRoots = [ "/var/lib/sparkles-backups" ];
        };
        maxClones = 1;
        compaction.auto = {
          minQuads = 5000;
          ratio = 0.1;
          idleSeconds = 0;
        };
      };
      # Dummy credentials only: listing providers does not contact their endpoints.
      systemd.services.sparkles.environment.TEST_OPENAI_API_KEY = "dummy-openai-key";
      environment.etc."sparkles/backup.toml" = {
        mode = "0400";
        user = "sparkles";
        text = ''
          version = 1

          [repositories.local]
          type = "fs"
          path = "/var/lib/sparkles-backups/local"
        '';
      };
      networking.hosts."127.0.0.1" = [ "sparkles.test" ];
      # commits keep --min-free-disk-mb (1 GiB) free on the data disk
      virtualisation.diskSize = 3072;
    };

  # the same behind nginx, with authentication and native TLS (the test certificate of the
  # server's unit tests, issued for localhost and 127.0.0.1)
  nodes.authed =
    { ... }:
    {
      imports = [ self.nixosModules.default ];
      services.sparkles = {
        enable = true;
        datasets.demo = { };
        nginx = {
          enable = true;
          virtualHost = "sparkles.test";
        };
        compaction.auto.enable = false;
        # anonymous callers keep full access; alice signs in with a password
        auth.configFile = "/etc/sparkles/auth.toml";
        tls = {
          certFile = "/etc/sparkles/tls/cert.pem";
          keyFile = "/etc/sparkles/tls/key.pem";
        };
      };
      environment.etc."sparkles/tls/ca.pem".source = ../crates/sparkles-server/src/tls/testdata/ca.pem;
      environment.etc."sparkles/tls/cert.pem".source =
        ../crates/sparkles-server/src/tls/testdata/cert-a.pem;
      environment.etc."sparkles/tls/key.pem" = {
        source = ../crates/sparkles-server/src/tls/testdata/key-a.pem;
        mode = "0440";
        user = "sparkles";
      };
      environment.etc."sparkles/auth.toml" = {
        mode = "0440";
        user = "sparkles";
        text = ''
          version = 1

          [anonymous]
          datasets = { "*" = "admin" }

          [[users]]
          name = "alice"
          datasets = { demo = "read" }
          password = "$argon2id$v=19$m=19456,t=2,p=1$Rx7LbFknfawV8XRp8CMm0Q$dEIdCOdMNBRhz+OBLTVscRhb7x5z8rlt9+wV1jOZBls"
        '';
      };
      networking.hosts."127.0.0.1" = [ "sparkles.test" ];
      # commits keep --min-free-disk-mb (1 GiB) free on the data disk
      virtualisation.diskSize = 3072;
    };

  testScript = ''
    import json
    import time

    def last_value(csv):
        return csv.strip().splitlines()[-1].strip()

    def wait_task(node, base, task):
        for _ in range(240):
            t = json.loads(node.succeed(f"curl -sf {base}/\\$/tasks/{task['id']}"))
            if t["state"] not in ("queued", "running"):
                return t
            time.sleep(0.5)
        raise Exception(f"task {task['id']} did not end")

    def post_json(node, url, body):
        return json.loads(
            node.succeed(
                f"curl -sf -X POST {url} -H 'Content-Type: application/json' -d '{json.dumps(body)}'"
            )
        )

    machine.wait_for_unit("sparkles.service")
    machine.wait_for_open_port(3030)
    machine.wait_for_unit("nginx.service")

    base = "http://sparkles.test"
    models = json.loads(machine.succeed(f"curl -sf {base}/\\$/models"))
    assert models["configured"], models
    providers = {p["name"]: p for p in models["providers"]}
    assert providers["claude"]["status"] == "secret-missing", providers
    assert providers["gateway"]["status"] == "ok", providers
    assert models["roles"]["extract"][0]["maxOutputTokens"] == 8192, models
    machine.succeed("install -o sparkles -g sparkles -m 0400 /dev/null /var/lib/sparkles/test-model-key")
    machine.succeed("printf '%s' dummy-anthropic-key > /var/lib/sparkles/test-model-key")
    models = json.loads(machine.succeed(f"curl -sf {base}/\\$/models"))
    assert next(p for p in models["providers"] if p["name"] == "claude")["status"] == "ok", models
    assert "dummy-anthropic-key" not in json.dumps(models) and "dummy-openai-key" not in json.dumps(models), models
    machine.succeed("truncate -s 0 /var/lib/sparkles/test-model-key")
    models = json.loads(machine.succeed(f"curl -sf {base}/\\$/models"))
    assert next(p for p in models["providers"] if p["name"] == "claude")["status"] == "secret-missing", models
    # Declared datasets receive settings before their first open. A settings-only
    # declaration must not block subsequent creation through the catalog API.
    assistant = json.loads(machine.succeed(f"curl -sf {base}/\\$/assistant/demo"))
    assert assistant["enabled"] and assistant["ingest"] and assistant["send"] == "documents", assistant
    memory = json.loads(machine.succeed(f"curl -sf {base}/\\$/memory/demo"))
    assert memory["agentGraphs"] == ["urn:x-sparkles:import/*"], memory
    assistant = json.loads(machine.succeed(f"curl -sf {base}/\\$/assistant/archive"))
    assert assistant["enabled"] and assistant["send"] == "rows", assistant
    machine.succeed("test ! -e /var/lib/sparkles/databases/later")
    post_json(machine, f"{base}/\\$/datasets", {"dbName": "later", "dbType": "persistent"})
    assistant = json.loads(machine.succeed(f"curl -sf {base}/\\$/assistant/later"))
    assert not assistant["enabled"], assistant
    machine.succeed(
        f"curl -sf -X PUT {base}/\\$/assistant/demo -H 'Content-Type: application/json' "
        + "-d '{\"enabled\":false}'"
    )
    machine.succeed(f"curl -sf {base}/\\$/ping")
    # a name the server is not known by (a DNS-rebinding page) is refused
    code = machine.succeed(
        "curl -s -o /dev/null -w '%{http_code}' -H 'Host: evil.example' http://127.0.0.1:3030/\\$/datasets"
    )
    assert code.strip() == "421", code
    machine.succeed(
        f"curl -sf {base}/demo/update --data-urlencode "
        + "'update=INSERT DATA { <urn:a> <urn:p> 42 . <urn:b> <urn:p> 7 }'"
    )
    out = machine.succeed(
        f"curl -sf {base}/demo/sparql -H 'Accept: text/csv' --data-urlencode "
        + "'query=SELECT (SUM(?v) AS ?s) WHERE { ?x <urn:p> ?v }'"
    )
    assert last_value(out) == "49", out

    # the automatic compaction policy and the clone limit come from the module's options
    c = json.loads(machine.succeed(f"curl -sf {base}/\\$/compaction/demo"))
    assert c["serverEnabled"], c
    assert c["policy"]["minDeltaQuads"] == 5000, c
    assert abs(c["policy"]["deltaRatio"] - 0.1) < 1e-9, c
    assert c["policy"]["idleSeconds"] == 0, c
    machine.succeed("systemctl show -p ExecStart sparkles.service | grep -q -- '--max-clones 1'")

    # the admin API and the embedded UI are reachable through the proxy
    machine.succeed(f"curl -sf {base}/\\$/datasets | grep -q scratch")
    machine.succeed(f"curl -sf {base}/ui/ | grep -qi '<html'")

    # persistent data survives a restart, the in-memory dataset does not
    machine.succeed(f"curl -sf {base}/scratch/update --data-urlencode 'update=INSERT DATA {{ <urn:m> <urn:p> 1 }}'")
    machine.systemctl("restart sparkles.service")
    machine.wait_for_open_port(3030)
    for name in ["demo", "later"]:
        assistant = json.loads(machine.succeed(f"curl -sf {base}/\\$/assistant/{name}"))
        assert assistant["enabled"] and assistant["ingest"] and assistant["send"] == "documents", assistant
    machine.succeed("test $(stat -c %a /var/lib/sparkles/databases/later/assistant.json) = 600")
    machine.succeed("test $(stat -c %U /var/lib/sparkles/databases/later/assistant.json) = sparkles")
    out = machine.succeed(
        f"curl -sf {base}/demo/sparql -H 'Accept: text/csv' --data-urlencode "
        + "'query=SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }'"
    )
    assert last_value(out) == "2", out
    out = machine.succeed(
        f"curl -sf {base}/scratch/sparql -H 'Accept: text/csv' --data-urlencode "
        + "'query=SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }'"
    )
    assert last_value(out) == "0", out

    # a backup into the fs repository of the backup config, restored as a new dataset
    repos = json.loads(machine.succeed(f"curl -sf {base}/\\$/repositories"))
    assert [r["name"] for r in repos["repositories"]] == ["local"], repos
    task = post_json(machine, f"{base}/\\$/backups/demo", {"repository": "local", "name": "vm1"})
    task = wait_task(machine, base, task)
    assert task["state"] == "done", task
    machine.succeed("test -f /var/lib/sparkles-backups/local/backups/vm1.json")
    task = post_json(
        machine, f"{base}/\\$/backups/demo/local/vm1/restore", {"target": "restored"}
    )
    task = wait_task(machine, base, task)
    assert task["state"] == "done", task
    out = machine.succeed(
        f"curl -sf {base}/restored/sparql -H 'Accept: text/csv' --data-urlencode "
        + "'query=SELECT (SUM(?v) AS ?s) WHERE { ?x <urn:p> ?v }'"
    )
    assert last_value(out) == "49", out
    # the config file is re-read on reload
    machine.succeed("systemctl reload sparkles.service")
    machine.succeed(f"curl -sf {base}/\\$/repositories/local | grep -q reachable")

    # the CLI is installed; the served database is locked against a second process
    machine.succeed("sparkles --version")
    err = machine.fail(
        "runuser -u sparkles -- sparkles stats --loc /var/lib/sparkles/declarative/demo 2>&1"
    )
    assert "in use by another process" in err, err
    # behind nginx each client has a budget of failed logins of its own (nginx names it in
    # X-Forwarded-For, the server trusts nginx), and a client's Forwarded header does not
    # choose the budget: the default preauth budget allows 60 failures at once
    authed.wait_for_unit("sparkles.service")
    authed.wait_for_open_port(3030)
    authed.wait_for_unit("nginx.service")
    c = json.loads(authed.succeed(f"curl -sf {base}/\\$/compaction/demo"))
    assert not c["serverEnabled"] and c["state"] == "off", c
    ask = f"'{base}/demo/sparql?query=ASK%7B%7D'"

    # the server speaks TLS itself: HTTP/2 and HTTP/1.1 through ALPN, nothing in clear
    tls = "--cacert /etc/sparkles/tls/ca.pem https://localhost:3030/\\$/ping"
    v = authed.succeed(f"curl -sf -o /dev/null -w '%{{http_version}}' --http2 {tls}")
    assert v.strip() == "2", v
    v = authed.succeed(f"curl -sf -o /dev/null -w '%{{http_version}}' --http1.1 {tls}")
    assert v.strip() == "1.1", v
    authed.fail("curl -sf http://127.0.0.1:3030/\\$/ping")
    # a reload re-reads the certificate and keeps serving
    authed.succeed("systemctl reload sparkles.service")
    authed.succeed(f"curl -sf {tls}")

    def status(args):
        return authed.succeed(f"curl -s -o /dev/null -w '%{{http_code}}' {args}").strip()

    # the budget refills at 30 a minute, so a slow machine gets a few more tries than 60
    spent = None
    for i in range(200):
        code = status(f"--interface 127.0.0.2 -u alice:wrong{i} -H 'Forwarded: for=198.51.100.{i}' {ask}")
        if code == "429":
            spent = i
            break
        assert code == "401", (i, code)
    assert spent is not None and spent >= 60, spent
    code = status(f"--interface 127.0.0.3 -u alice:alice-pw -H 'Forwarded: for=127.0.0.2' {ask}")
    assert code == "200", code
    # the spent client is refused password checks only: health checks and anonymous
    # requests still pass
    assert status(f"--interface 127.0.0.2 {base}/\\$/ping") == "200"
    assert status(f"--interface 127.0.0.2 {ask}") == "200"

  '';
}
