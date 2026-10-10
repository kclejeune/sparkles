#!/usr/bin/env bash
# Install the Helm chart (deploy/helm/sparkles) into a kind cluster with a locally built
# image, and check that it serves: wait until the pod is ready, load data and run a SPARQL
# query through the Service, then change the settings ConfigMap and wait for the server to
# reload it through --watch-config, without a restart.
#
#   scripts/helm-smoke.sh [IMAGE]
#
# IMAGE defaults to sparkles:smoke, built from the Dockerfile unless SKIP_BUILD=1 says it
# exists. Needs docker, kind, kubectl and helm. KIND_CLUSTER names the cluster (default
# sparkles-smoke); one the script creates is deleted at the end unless KEEP_CLUSTER=1.
# The query goes through `kubectl port-forward` on SMOKE_PORT (default 48080).
set -euo pipefail

image=${1:-sparkles:smoke}
cluster=${KIND_CLUSTER:-sparkles-smoke}
port=${SMOKE_PORT:-48080}
ns=sparkles-smoke
release=smoke
root="$(cd "$(dirname "$0")/.." && pwd)"

for tool in docker kind kubectl helm curl; do
  command -v "$tool" > /dev/null || {
    echo "helm-smoke: $tool is not installed" >&2
    exit 2
  }
done

created=0
pf=""
cleanup() {
  if [[ -n $pf ]]; then kill "$pf" 2> /dev/null || true; fi
  if ((created)) && [[ -z ${KEEP_CLUSTER:-} ]]; then
    kind delete cluster --name "$cluster" > /dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

if [[ -z ${SKIP_BUILD:-} ]]; then
  docker build -t "$image" "$root"
fi
if ! kind get clusters 2> /dev/null | grep -qx "$cluster"; then
  kind create cluster --name "$cluster" --wait 120s
  created=1
fi
kctx="kind-$cluster"
k() { kubectl --context "$kctx" -n "$ns" "$@"; }

kind load docker-image "$image" --name "$cluster"
kubectl --context "$kctx" create namespace "$ns" --dry-run=client -o yaml |
  kubectl --context "$kctx" apply -f - > /dev/null

repo=${image%:*}
tag=${image##*:}
helm --kube-context "$kctx" upgrade --install "$release" "$root/deploy/helm/sparkles" \
  --namespace "$ns" \
  --set image.repository="$repo" --set image.tag="$tag" --set image.pullPolicy=Never \
  --set server.allowOpenNetwork=true \
  --set persistence.size=1Gi \
  --set-json 'settings={"defaults":{"assistant":{"enabled":false}}}' \
  --wait --timeout 5m

pod="$release-sparkles-0"
k wait --for=condition=Ready "pod/$pod" --timeout=180s
echo "helm-smoke: $pod is ready"
# the root filesystem is read-only and the server runs as uid 10001
[[ $(k get pod "$pod" -o jsonpath='{.spec.containers[0].securityContext.readOnlyRootFilesystem}') == true ]]

k port-forward "svc/$release-sparkles" "$port:3030" > /dev/null 2>&1 &
pf=$!
base="http://127.0.0.1:$port"
for _ in $(seq 1 50); do
  curl -fsS "$base/\$/ping" > /dev/null 2>&1 && break
  sleep 0.2
done

curl -fsS -X POST "$base/\$/datasets" --data 'dbName=smoke&dbType=tdb2' > /dev/null
curl -fsS -X POST "$base/smoke/data" -H 'Content-Type: text/turtle' \
  --data-binary '<http://ex.org/a> <http://ex.org/p> "hello" .' > /dev/null
answer=$(curl -fsS "$base/smoke/sparql" -H 'Accept: application/sparql-results+json' \
  --data-urlencode 'query=SELECT ?o { ?s ?p ?o }')
grep -q '"hello"' <<< "$answer" || {
  echo "helm-smoke: unexpected query answer: $answer" >&2
  exit 1
}
echo "helm-smoke: SPARQL query answered"

# Change the settings ConfigMap in place, as `helm upgrade` would, and wait for the
# server to log the reload. The kubelet updates the volume within its sync period
# (about a minute), so this can take a while.
restarts=$(k get pod "$pod" -o jsonpath='{.status.containerStatuses[0].restartCount}')
k patch configmap "$release-sparkles" --type merge \
  -p '{"data":{"settings.json":"{\"defaults\":{\"assistant\":{\"enabled\":true}}}"}}' > /dev/null
for i in $(seq 1 120); do
  if k logs "$pod" | grep -q 'settings reloaded'; then
    echo "helm-smoke: settings reloaded after the ConfigMap change (${i}x2 s)"
    break
  fi
  if ((i == 120)); then
    echo "helm-smoke: no reload within 4 minutes" >&2
    k logs "$pod" | tail -20 >&2
    exit 1
  fi
  sleep 2
done
k logs "$pod" | grep -q 'configuration changed' || {
  echo "helm-smoke: the reload did not come from --watch-config" >&2
  exit 1
}
[[ $(k get pod "$pod" -o jsonpath='{.status.containerStatuses[0].restartCount}') == "$restarts" ]] || {
  echo "helm-smoke: the pod restarted" >&2
  exit 1
}
echo "helm-smoke: passed"
