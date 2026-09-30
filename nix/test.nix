# NixOS VM test: the service starts, serves a declared dataset through nginx, keeps
# data across restarts, serves the embedded UI, and tells clients behind nginx apart
# for the failed-login budget.
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
          scratch.type = "mem";
        };
        nginx = {
          enable = true;
          virtualHost = "sparkles.test";
        };
        # anonymous callers keep full access; alice signs in with a password
        auth.configFile = "/etc/sparkles/auth.toml";
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
          password = "$argon2id$v=19$m=19456,t=2,p=1$Rx7LbFknfawV8XRp8CMm0Q$dEIdCOdMNBRhz+OBLTVscRhb7x5z8rlt9+wV1jOZBls"
        '';
      };
      networking.hosts."127.0.0.1" = [ "sparkles.test" ];
    };

  testScript = ''
    def last_value(csv):
        return csv.strip().splitlines()[-1].strip()

    machine.wait_for_unit("sparkles.service")
    machine.wait_for_open_port(3030)
    machine.wait_for_unit("nginx.service")

    base = "http://sparkles.test"
    machine.succeed(f"curl -sf {base}/\\$/ping")
    machine.succeed(
        f"curl -sf {base}/demo/update --data-urlencode "
        + "'update=INSERT DATA { <urn:a> <urn:p> 42 . <urn:b> <urn:p> 7 }'"
    )
    out = machine.succeed(
        f"curl -sf {base}/demo/sparql -H 'Accept: text/csv' --data-urlencode "
        + "'query=SELECT (SUM(?v) AS ?s) WHERE { ?x <urn:p> ?v }'"
    )
    assert last_value(out) == "49", out

    # the admin API and the embedded UI are reachable through the proxy
    machine.succeed(f"curl -sf {base}/\\$/datasets | grep -q scratch")
    machine.succeed(f"curl -sf {base}/ui/ | grep -qi '<html'")

    # persistent data survives a restart, the in-memory dataset does not
    machine.succeed(f"curl -sf {base}/scratch/update --data-urlencode 'update=INSERT DATA {{ <urn:m> <urn:p> 1 }}'")
    machine.systemctl("restart sparkles.service")
    machine.wait_for_open_port(3030)
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

    # behind nginx each client has a budget of failed logins of its own (nginx names it in
    # X-Forwarded-For, the server trusts nginx), and a client's Forwarded header does not
    # choose the budget: the default preauth budget allows 60 failures at once
    ask = f"'{base}/demo/sparql?query=ASK%7B%7D'"

    def status(args):
        return machine.succeed(f"curl -s -o /dev/null -w '%{{http_code}}' {args}").strip()

    for i in range(60):
        code = status(f"--interface 127.0.0.2 -u alice:wrong{i} -H 'Forwarded: for=198.51.100.{i}' {ask}")
        assert code == "401", (i, code)
    code = status(f"--interface 127.0.0.2 -u alice:wrong -H 'Forwarded: for=198.51.100.99' {ask}")
    assert code == "429", code
    code = status(f"--interface 127.0.0.3 -u alice:alice-pw -H 'Forwarded: for=127.0.0.2' {ask}")
    assert code == "200", code

    # the CLI is installed; the served database is locked against a second process
    machine.succeed("sparkles --version")
    err = machine.fail(
        "runuser -u sparkles -- sparkles stats --loc /var/lib/sparkles/declarative/demo 2>&1"
    )
    assert "in use by another process" in err, err
  '';
}
