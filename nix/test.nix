# NixOS VM test: the service starts, serves a declared dataset through nginx, keeps
# data across restarts, and serves the embedded UI.
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

    # the CLI is installed; the served database is locked against a second process
    machine.succeed("sparkles --version")
    err = machine.fail(
        "runuser -u sparkles -- sparkles stats --loc /var/lib/sparkles/declarative/demo 2>&1"
    )
    assert "in use by another process" in err, err
  '';
}
