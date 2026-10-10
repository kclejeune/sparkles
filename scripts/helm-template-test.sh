#!/usr/bin/env bash
# Lint the Helm chart (deploy/helm/sparkles) with each of its ci/*-values.yaml files, as
# chart-testing does, render it, and check what the templates produce for those values
# and for combinations that must be refused. Needs helm and yq (mikefarah's, v4).
set -euo pipefail

chart="$(cd "$(dirname "$0")/../deploy/helm/sparkles" && pwd)"
failures=0

fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}

render() {
  helm template t "$chart" --namespace db "$@"
}

# the StatefulSet's server container args, one per line
server_args() {
  yq 'select(.kind == "StatefulSet") | .spec.template.spec.containers[0].args[]'
}

has_arg() {
  local out=$1 want=$2
  grep -qxF -- "$want" <<< "$out" || fail "$3: no argument '$want'"
}

lacks_arg() {
  local out=$1 unwanted=$2
  if grep -qxF -- "$unwanted" <<< "$out"; then fail "$3: unexpected argument '$unwanted'"; fi
}

for f in "$chart"/ci/*-values.yaml; do
  name=$(basename "$f")
  helm lint --strict "$chart" -f "$f" > /dev/null || fail "helm lint with $name"
  render -f "$f" > /dev/null || fail "helm template with $name"
done

# defaults: refused without auth or allowOpenNetwork
if render > /dev/null 2>&1; then fail "defaults rendered without auth or allowOpenNetwork"; fi

# open: one replica, RWO claim, read-only root, probes, Service names as public hosts,
# --watch-config with a settings file
out=$(render -f "$chart/ci/open-values.yaml")
sts=$(yq 'select(.kind == "StatefulSet")' <<< "$out")
[[ $(yq '.spec.replicas' <<< "$sts") == 1 ]] || fail "open: replicas"
[[ $(yq '.spec.volumeClaimTemplates[0].spec.accessModes[0]' <<< "$sts") == ReadWriteOnce ]] || fail "open: access mode"
[[ $(yq '.spec.template.spec.containers[0].securityContext.readOnlyRootFilesystem' <<< "$sts") == true ]] || fail "open: read-only root"
[[ $(yq '.spec.template.spec.containers[0].readinessProbe.httpGet.path' <<< "$sts") == '/$/ready' ]] || fail "open: readiness"
[[ $(yq '.spec.template.spec.containers[0].startupProbe.failureThreshold' <<< "$sts") == 360 ]] || fail "open: startup budget"
[[ $(yq '.spec.template.spec.volumes[] | select(.name == "tmp") | has("emptyDir")' <<< "$sts") == true ]] || fail "open: /tmp volume"
[[ $(yq '.spec.template.spec.volumes[] | select(.name == "config") | .configMap.name' <<< "$sts") == t-sparkles ]] || fail "open: config volume"
if yq '.spec.template.spec.containers[0].volumeMounts[] | select(.name == "config") | has("subPath")' <<< "$sts" | grep -q true; then
  fail "open: the ConfigMap is mounted with subPath, which Kubernetes never updates"
fi
args=$(server_args <<< "$out")
has_arg "$args" --watch-config open
has_arg "$args" /etc/sparkles/config/settings.json open
has_arg "$args" t-sparkles.db.svc.cluster.local open
lacks_arg "$args" --models-dir open
lacks_arg "$args" --models-download open
[[ $(yq 'select(.kind == "ConfigMap") | .data["settings.json"]' <<< "$out" | yq -p json '.defaults.assistant.enabled') == false ]] || fail "open: settings.json"
if yq 'select(.kind == "StatefulSet") | .spec.template.spec.initContainers' <<< "$out" | grep -qv null; then
  fail "open: init containers without model delivery"
fi

# full: secrets stay out of the ConfigMap; keys become file references
out=$(render -f "$chart/ci/full-values.yaml")
args=$(server_args <<< "$out")
has_arg "$args" "gw=file:/run/secrets/sparkles/models/model-keys/gw" full
has_arg "$args" "other=file:/run/secrets/sparkles/models/model-keys/other" full
has_arg "$args" /etc/sparkles/auth/auth.toml full
has_arg "$args" /etc/sparkles/backup/backup.toml full
has_arg "$args" /etc/sparkles/config/rate-limits.json full
has_arg "$args" /etc/sparkles/config/models.json full
has_arg "$args" sparql.example.org full
has_arg "$args" sparkles.internal full
cm=$(yq 'select(.kind == "ConfigMap")' <<< "$out")
if grep -q argon2id <<< "$cm"; then fail "full: the auth configuration is in the ConfigMap"; fi
[[ $(yq 'select(.kind == "Secret" and .metadata.name == "t-sparkles-auth") | .data["auth.toml"]' <<< "$out" | base64 -d | grep -c argon2id) == 1 ]] || fail "full: inline auth Secret"
[[ $(yq 'select(.kind == "StatefulSet") | .spec.template.spec.volumes[] | select(.name == "backup") | .secret.secretName' <<< "$out") == sparkles-backup ]] || fail "full: backup Secret"
[[ $(yq 'select(.kind == "StatefulSet") | .spec.template.spec.volumes[] | select(.name == "model-secret-*") | .name' <<< "$out" | wc -l) == 1 ]] || fail "full: one volume per model Secret"
[[ $(yq 'select(.kind == "StatefulSet") | .spec.template.metadata.annotations["checksum/config"]' <<< "$out") =~ ^[0-9a-f]{64}$ ]] || fail "full: checksum annotation"
[[ $(yq 'select(.kind == "PodDisruptionBudget") | .spec.maxUnavailable' <<< "$out") == 0 ]] || fail "full: PDB"
[[ $(yq 'select(.kind == "Ingress") | .spec.rules[0].host' <<< "$out") == sparql.example.org ]] || fail "full: Ingress"
before=$(yq 'select(.kind == "StatefulSet") | .spec.template.metadata.annotations["checksum/config"]' <<< "$out")
after=$(render -f "$chart/ci/full-values.yaml" --set-json 'settings={"defaults":{}}' | yq 'select(.kind == "StatefulSet") | .spec.template.metadata.annotations["checksum/config"]')
[[ $before != "$after" ]] || fail "full: the checksum does not follow the settings"

# pull: init container with the manifest, server downloads off, store read-only
out=$(render -f "$chart/ci/models-pull-values.yaml")
init=$(yq 'select(.kind == "StatefulSet") | .spec.template.spec.initContainers[0].args | join(" ")' <<< "$out")
[[ $init == "models pull --manifest /etc/sparkles/config/models-manifest.json --dir /models" ]] || fail "pull: init args: $init"
args=$(server_args <<< "$out")
has_arg "$args" --models-dir pull
has_arg "$args" off pull
[[ $(yq 'select(.kind == "StatefulSet") | .spec.template.spec.containers[0].volumeMounts[] | select(.name == "models") | .readOnly' <<< "$out") == true ]] || fail "pull: read-only store"
[[ $(yq 'select(.kind == "PersistentVolumeClaim") | .metadata.name' <<< "$out") == t-sparkles-models ]] || fail "pull: models PVC"
[[ $(yq 'select(.kind == "ConfigMap") | .data["models-manifest.json"]' <<< "$out" | yq -p json '.models[0].repo') == Qwen/Qwen3-Embedding-0.6B ]] || fail "pull: manifest"

# image: an image volume with the store's subPath; OCR variant with its models; HTTPRoute
out=$(render -f "$chart/ci/models-image-values.yaml")
sts=$(yq 'select(.kind == "StatefulSet")' <<< "$out")
[[ $(yq '.spec.template.spec.volumes[] | select(.name == "models") | .image.reference' <<< "$sts") == registry.example.org/sparkles-models:qwen3-embedding-0.6b ]] || fail "image: image volume"
[[ $(yq '.spec.template.spec.containers[0].volumeMounts[] | select(.name == "models") | .subPath' <<< "$sts") == models ]] || fail "image: subPath"
[[ $(yq '.spec.template.spec.containers[0].image' <<< "$sts") == sparkles:0.1.0-ocr ]] || fail "image: OCR image"
args=$(server_args <<< "$out")
has_arg "$args" --pdf-ocr-models image
has_arg "$args" /ocr-models image
[[ $(yq '.spec.template.spec.volumes[] | select(.name == "ocr-models") | .persistentVolumeClaim.claimName' <<< "$sts") == pp-ocr-models ]] || fail "image: OCR models"
[[ $(yq 'select(.kind == "HTTPRoute") | .spec.hostnames[0]' <<< "$out") == sparql.example.org ]] || fail "image: HTTPRoute"
has_arg "$args" sparql.example.org image

# download: downloads on, the store on its own volume, writable
out=$(render -f "$chart/ci/models-download-values.yaml")
args=$(server_args <<< "$out")
has_arg "$args" --models-download download
has_arg "$args" on download
has_arg "$args" /models download
[[ $(yq 'select(.kind == "StatefulSet") | .spec.template.spec.containers[0].volumeMounts[] | select(.name == "models") | .readOnly' <<< "$out") == false ]] || fail "download: writable store"
# download without its own volume: the default store on the data volume
args=$(render --set server.allowOpenNetwork=true --set models.delivery=download | server_args)
has_arg "$args" --models-download download-default
lacks_arg "$args" --models-dir download-default

# imageCopy: an init container from the model image, data in an emptyDir
out=$(render -f "$chart/ci/models-copy-values.yaml")
[[ $(yq 'select(.kind == "StatefulSet") | .spec.template.spec.initContainers[0].image' <<< "$out") == registry.example.org/sparkles-models:qwen3-embedding-0.6b ]] || fail "imageCopy: init image"
[[ $(yq 'select(.kind == "StatefulSet") | .spec.template.spec.volumes[] | select(.name == "data") | has("emptyDir")' <<< "$out") == true ]] || fail "imageCopy: data emptyDir"
[[ $(yq 'select(.kind == "StatefulSet") | .spec | has("volumeClaimTemplates")' <<< "$out") == false ]] || fail "imageCopy: no claim template"

# watchConfig off, or nothing to watch: no --watch-config
args=$(render -f "$chart/ci/open-values.yaml" --set server.watchConfig=false | server_args)
lacks_arg "$args" --watch-config watch-off
args=$(render --set server.allowOpenNetwork=true | server_args)
lacks_arg "$args" --watch-config nothing-to-watch

# refused combinations
refuse() {
  if render "$@" > /dev/null 2>&1; then fail "rendered: $*"; fi
}
refuse --set server.allowOpenNetwork=true --set terminationGracePeriodSeconds=20
refuse --set auth.enabled=true
refuse --set server.allowOpenNetwork=true --set models.delivery=image
refuse --set server.allowOpenNetwork=true --set models.delivery=pull
refuse --set server.allowOpenNetwork=true --set ocr.enabled=true
refuse --set server.allowOpenNetwork=true --set models.delivery=sometimes
refuse --set server.allowOpenNetwork=true --set-json 'models.manifest={"models":[{"repo":"a/b","revision":"main"}]}' --set models.delivery=pull
refuse --set server.allowOpenNetwork=true --set no.such=value

if ((failures)); then
  echo "$failures check(s) failed" >&2
  exit 1
fi
echo "helm chart: all template checks passed"
