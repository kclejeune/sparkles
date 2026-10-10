# Sparkles Helm chart

This chart runs the Sparkles server on Kubernetes as a StatefulSet with one pod and a
ReadWriteOnce data volume. Sparkles is a single writer on local disk, so the chart never
runs more than one replica. [docs/USAGE.md](../../../docs/USAGE.md#kubernetes-helm)
explains installation, configuration, model delivery, OCR, reloads, upgrades, backups and
sizing. This file lists the values.

```sh
helm install sparkles deploy/helm/sparkles -n sparkles --create-namespace \
  --set image.repository=registry.example.org/sparkles --set image.tag=0.1.0 \
  -f my-values.yaml
```

The chart refuses to render until you choose between authentication (`auth.enabled`)
and an open server (`server.allowOpenNetwork=true`).

## Values

| Key | Default | Description |
|---|---|---|
| `image.repository` | `sparkles` | The server image. The project publishes none, so build and push your own. |
| `image.tag` | the chart's appVersion | The image tag. |
| `image.pullPolicy` | `IfNotPresent` | |
| `imagePullSecrets` | `[]` | Pull secrets for private registries. |
| `serviceAccount.create` | `true` | Create a ServiceAccount. |
| `serviceAccount.name` | the full name | |
| `serviceAccount.automountServiceAccountToken` | `false` | The server never calls the Kubernetes API. |
| `podAnnotations`, `podLabels` | `{}` | |
| `podSecurityContext` | uid and gid 10001, non-root, `RuntimeDefault` seccomp | |
| `securityContext` | read-only root filesystem, no privilege escalation, all capabilities dropped | |
| `server.port` | `3030` | The port in the pod. |
| `server.logFormat` | `json` | `--log-format`. |
| `server.logLevel` | the server's default | `RUST_LOG`. |
| `server.shutdownGrace` | `20` | `--shutdown-grace` in seconds. |
| `server.allowOpenNetwork` | `false` | Serve without authentication (`SPARKLES_ALLOW_OPEN_NETWORK=1`). |
| `server.publicHosts` | `[]` | More `--public-host` names. The Service names and the Ingress and HTTPRoute hosts are added on their own. |
| `server.clusterDomain` | `cluster.local` | The DNS domain of the Service names. |
| `server.watchConfig` | `true` | `--watch-config`, which reloads the configuration files when Kubernetes updates their volumes. |
| `server.extraArgs` | `[]` | More `serve` arguments. |
| `server.extraEnv`, `server.extraEnvFrom` | `[]` | More environment for the server and the `pull` init container. |
| `terminationGracePeriodSeconds` | `40` | Must be longer than `server.shutdownGrace`. |
| `restartOnConfigChange` | `false` | Restart the pod when the rendered ConfigMap or inline Secrets change. |
| `settings` | `{}` | Declared dataset settings, rendered as `settings.json` (`--settings`). |
| `rateLimits` | `{}` | The rate-limit configuration, rendered as `rate-limits.json` (`--rate-limit-config`). |
| `models.config` | `{}` | The model configuration, rendered as `models.json` (`--model-config`). |
| `models.secrets` | `{}` | `NAME: {secretName, key}` entries, each passed as `--model-secret NAME=file:...` from a mounted Secret. |
| `models.delivery` | `none` | `none`, `download`, `pull`, `image` or `imageCopy`. Every value other than `none` needs a build with `sparkles models`. |
| `models.mountPath` | `/models` | Where the model store is mounted (`--models-dir`). |
| `models.manifest` | `{models: []}` | The manifest of `pull`, rendered as `models-manifest.json`. |
| `models.persistence.enabled` | `false` | A PVC of its own for the store. |
| `models.persistence.size` | `10Gi` | |
| `models.persistence.storageClass` | `""` | |
| `models.persistence.accessModes` | `[ReadWriteOnce]` | |
| `models.persistence.existingClaim` | `""` | |
| `models.image.reference` | `""` | The OCI image of `image` and `imageCopy`. |
| `models.image.pullPolicy` | `IfNotPresent` | |
| `models.image.path` | `/models` | The store's directory in that image. |
| `models.initResources` | `{}` | Resources of the `pull` and `imageCopy` init containers. |
| `ocr.enabled` | `false` | Run the OCR image and pass `--pdf-ocr-models`. |
| `ocr.image.repository` | `image.repository` | |
| `ocr.image.tag` | the server tag with `-ocr` | |
| `ocr.models.mountPath` | `/ocr-models` | |
| `ocr.models.existingClaim` | `""` | A PVC with the PP-OCR models. |
| `ocr.models.subPath` | `""` | Their directory in that PVC. |
| `ocr.models.imageReference` | `""` | Or an OCI image of them, mounted as an image volume. |
| `ocr.models.imagePullPolicy` | `IfNotPresent` | |
| `auth.enabled` | `false` | `--auth-config`. |
| `auth.config` | `""` | Inline TOML, rendered into a Secret. |
| `auth.existingSecret` | `""` | An existing Secret with the file. |
| `auth.existingConfigMap` | `""` | An existing ConfigMap with the file, for one without hashes. |
| `auth.key` | `auth.toml` | The key of the file. |
| `backup.enabled` | `false` | `--backup-config`. |
| `backup.config`, `backup.existingSecret`, `backup.existingConfigMap`, `backup.key` | | As for `auth`, with the key `backup.toml`. |
| `persistence.enabled` | `true` | The data on a PVC. Without it the data is in an emptyDir. |
| `persistence.accessModes` | `[ReadWriteOnce]` | |
| `persistence.size` | `20Gi` | |
| `persistence.storageClass` | `""` | |
| `persistence.existingClaim` | `""` | |
| `persistence.annotations` | `{}` | |
| `persistence.retentionPolicy` | `Retain` on delete and scale-down | |
| `tmp.sizeLimit`, `tmp.medium` | `""` | The emptyDir at `/tmp`, where large request bodies are spooled. |
| `service.type` | `ClusterIP` | |
| `service.port` | `3030` | |
| `service.annotations` | `{}` | |
| `ingress.enabled` | `false` | |
| `ingress.className`, `ingress.annotations`, `ingress.hosts`, `ingress.tls` | | |
| `httpRoute.enabled` | `false` | A Gateway API HTTPRoute. |
| `httpRoute.parentRefs`, `httpRoute.hostnames`, `httpRoute.rules`, `httpRoute.annotations` | | |
| `podDisruptionBudget.enabled` | `false` | |
| `podDisruptionBudget.maxUnavailable` | `0` | With one pod, this blocks node drains. |
| `resources` | requests 500m and 1Gi, limit 4Gi | |
| `probes.scheme` | `HTTP` | `HTTPS` with `--tls-cert`. |
| `probes.startup` | every 5 s, 360 failures | 30 minutes to open the datasets. |
| `probes.readiness` | every 10 s, 3 failures | `GET /$/ready`. |
| `probes.liveness` | every 30 s, 4 failures | `GET /$/ping`. |
| `nodeSelector`, `tolerations`, `affinity` | | |
| `extraVolumes`, `extraVolumeMounts` | `[]` | Such as a backup directory. |

## Tests

`scripts/helm-template-test.sh` lints the chart with each `ci/*-values.yaml` file and
checks the rendered manifests. `scripts/helm-smoke.sh` installs the chart into a kind
cluster with a locally built image and checks a query and a configuration reload.
