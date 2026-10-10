# NixOS VM test: the service starts, serves a declared dataset through nginx, keeps
# data across restarts, serves the embedded UI, and backs a dataset up into an `fs`
# repository of its backup config and restores it; with authentication and native TLS
# (node `authed`), it serves HTTP/1.1 and HTTP/2 over TLS, nginx proxies to it over https,
# and it tells clients behind nginx apart for the failed-login budget. The automatic
# compaction and clone options reach the server: a policy on one node, compaction turned
# off on the other. The declared settings apply to declared, in-memory and later
# datasets, runtime changes survive a reload and a restart, locks and declared datasets
# are refused at runtime, and a switch to other settings and model roles reloads the
# server without restarting it.
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
        # the assistant on for every dataset with `send` locked, and entries by name for
        # a declared dataset, an in-memory one and one that the API creates later
        settings = {
          defaults = {
            assistant.enabled = true;
            locked = [ "assistant.send" ];
          };
          datasets = {
            demo = {
              assistant = {
                ingest = true;
                send = "documents";
                historyDays = 30;
              };
              memory.agentGraphs = [ "urn:x-sparkles:import/*" ];
            };
            scratch.assistant.historyDays = 3;
            later.assistant.historyDays = 14;
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

      # a change of the declared settings and of the model settings, which a switch to
      # this configuration applies with a reload
      specialisation.changed.configuration = {
        services.sparkles.settings.datasets.archive.assistant.historyDays = 9;
        services.sparkles.models.settings.roles.summarize = [
          {
            provider = "gateway";
            model = "gpt-5-mini";
          }
        ];
      };
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
    # the declared settings: `defaults` for every dataset, an entry by name, a lock
    def kind(ds, k="assistant"):
        return json.loads(machine.succeed(f"curl -sf {base}/\\$/settings/{ds}/{k}"))

    def request(method, url, body=None):
        data = f"-H 'Content-Type: application/json' -d '{json.dumps(body)}'" if body is not None else ""
        out = machine.succeed(f"curl -s -X {method} -w '\\n%{{http_code}}' {data} '{url}'")
        text, code = out.rsplit("\n", 1)
        return int(code), (json.loads(text) if text.strip() else None)

    a = kind("demo")
    assert a["effective"]["enabled"] and a["effective"]["ingest"], a
    assert a["effective"]["send"] == "documents" and a["effective"]["historyDays"] == 30, a
    assert a["sources"]["enabled"] == "declared" and a["sources"]["historyDays"] == "declared", a
    assert a["sources"]["send"] == "locked" and a["locked"] == ["send"], a
    m = kind("demo", "memory")
    assert m["effective"]["agentGraphs"] == ["urn:x-sparkles:import/*"], m
    assert m["sources"]["agentGraphs"] == "declared", m
    # the legacy route answers the effective object
    legacy = json.loads(machine.succeed(f"curl -sf {base}/\\$/assistant/demo"))
    assert legacy["enabled"] and legacy["send"] == "documents", legacy
    # a dataset without an entry gets the defaults, with `send` locked at its default
    a = kind("archive")
    assert a["effective"]["enabled"] and a["sources"]["enabled"] == "declared", a
    assert a["effective"]["send"] == "schema" and a["sources"]["send"] == "locked", a
    # an in-memory dataset gets its declared values too (A5)
    a = kind("scratch")
    assert a["effective"]["historyDays"] == 3 and a["sources"]["historyDays"] == "declared", a
    assert a["effective"]["enabled"] and a["sources"]["enabled"] == "declared", a
    # a dataset that the API creates after the start gets its entry without a restart (A1)
    machine.succeed("test ! -e /var/lib/sparkles/databases/later")
    post_json(machine, f"{base}/\\$/datasets", {"dbName": "later", "dbType": "persistent"})
    a = kind("later")
    assert a["effective"]["historyDays"] == 14 and a["sources"]["historyDays"] == "declared", a
    assert a["effective"]["enabled"] and a["sources"]["enabled"] == "declared", a
    # the module writes nothing into the dataset directories
    machine.succeed("test ! -e /var/lib/sparkles/declarative/demo/assistant.json")
    machine.succeed("test ! -e /var/lib/sparkles/databases/later/assistant.json")

    # a runtime change overrides a declared value (A2), and a locked field is refused
    code, body = request("PATCH", f"{base}/$/settings/demo/assistant", {"historyDays": 7})
    assert code == 200, (code, body)
    code, body = request("PATCH", f"{base}/$/settings/demo/assistant", {"send": "rows"})
    assert code == 409 and body["code"] == "locked-by-config", (code, body)
    code, body = request("PATCH", f"{base}/$/settings/later/assistant", {"send": "rows"})
    assert code == 409 and body["code"] == "locked-by-config", (code, body)
    code, body = request("PATCH", f"{base}/$/settings/later/assistant", {"enabled": False})
    assert code == 200, (code, body)
    a = kind("demo")
    assert a["effective"]["historyDays"] == 7 and a["sources"]["historyDays"] == "runtime", a
    # the operator's datasets cannot be deleted through the API (A9)
    code, body = request("DELETE", f"{base}/$/datasets/demo")
    assert code == 409 and body["code"] == "declared-dataset", (code, body)
    code, body = request("DELETE", f"{base}/$/datasets/scratch")
    assert code == 409 and body["code"] == "declared-dataset", (code, body)
    # the runtime values survive a reload
    pid = machine.succeed("systemctl show -p MainPID --value sparkles.service").strip()
    machine.succeed("systemctl reload sparkles.service")
    assert machine.succeed("systemctl show -p MainPID --value sparkles.service").strip() == pid
    a = kind("demo")
    assert a["effective"]["historyDays"] == 7 and a["sources"]["historyDays"] == "runtime", a
    assert not kind("later")["effective"]["enabled"]
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
    # a reload during the start waits for the server instead of stopping it
    pid = machine.succeed("systemctl show -p MainPID --value sparkles.service").strip()
    machine.succeed("systemctl reload sparkles.service")
    machine.wait_for_open_port(3030)
    assert machine.succeed("systemctl show -p MainPID --value sparkles.service").strip() == pid
    # the runtime values survive a restart, and clearing one brings the declared value back
    a = kind("demo")
    assert a["effective"]["historyDays"] == 7 and a["sources"]["historyDays"] == "runtime", a
    a = kind("later")
    assert not a["effective"]["enabled"] and a["sources"]["enabled"] == "runtime", a
    code, body = request("DELETE", f"{base}/$/settings/demo/assistant?field=historyDays")
    assert code == 200, (code, body)
    a = kind("demo")
    assert a["effective"]["historyDays"] == 30 and a["sources"]["historyDays"] == "declared", a
    # the in-memory dataset starts with its declared values again
    assert kind("scratch")["effective"]["historyDays"] == 3
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

    # a switch to a configuration with other settings and model roles reloads the
    # server without a restart (A10), and keeps the runtime values
    pid = machine.succeed("systemctl show -p MainPID --value sparkles.service").strip()
    assert kind("archive")["effective"].get("historyDays") in (None, 30)
    roles = json.loads(machine.succeed(f"curl -sf {base}/\\$/models"))["roles"]
    assert roles.get("summarize") in (None, []), roles
    out = machine.succeed(
        "/run/current-system/specialisation/changed/bin/switch-to-configuration test 2>&1"
    )
    def units(verb):
        return [
            line for line in out.splitlines()
            if line.startswith(f"{verb} the following units:") and "sparkles.service" in line
        ]

    assert units("reloading"), out
    assert not units("restarting") and not units("stopping"), out
    machine.wait_until_succeeds(
        f"curl -sf {base}/\\$/settings/archive/assistant | grep -q '\"historyDays\":9'"
    )
    assert machine.succeed("systemctl show -p MainPID --value sparkles.service").strip() == pid
    a = kind("archive")
    assert a["sources"]["historyDays"] == "declared", a
    models = json.loads(machine.succeed(f"curl -sf {base}/\\$/models"))
    assert models["roles"]["summarize"][0]["provider"] == "gateway", models
    assert not kind("later")["effective"]["enabled"]

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
