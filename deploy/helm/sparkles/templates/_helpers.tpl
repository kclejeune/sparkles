{{/* The chart name. */}}
{{- define "sparkles.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* The full name of the release's resources. */}}
{{- define "sparkles.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{- define "sparkles.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "sparkles.labels" -}}
helm.sh/chart: {{ include "sparkles.chart" . }}
{{ include "sparkles.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "sparkles.selectorLabels" -}}
app.kubernetes.io/name: {{ include "sparkles.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "sparkles.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "sparkles.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/* The server image: the OCR variant with ocr.enabled. */}}
{{- define "sparkles.image" -}}
{{- $tag := default .Chart.AppVersion .Values.image.tag }}
{{- if .Values.ocr.enabled }}
{{- $repo := default .Values.image.repository .Values.ocr.image.repository }}
{{- printf "%s:%s" $repo (default (printf "%s-ocr" $tag) .Values.ocr.image.tag) }}
{{- else }}
{{- printf "%s:%s" .Values.image.repository $tag }}
{{- end }}
{{- end }}

{{/* Whether the chart renders a ConfigMap of configuration files. */}}
{{- define "sparkles.hasConfigMap" -}}
{{- if or .Values.settings .Values.rateLimits .Values.models.config (eq .Values.models.delivery "pull") }}true{{ end }}
{{- end }}

{{/* Where the auth configuration comes from: secret, configMap or inline (a Secret the chart renders). */}}
{{- define "sparkles.authSource" -}}
{{- if .Values.auth.existingSecret }}secret{{ else if .Values.auth.existingConfigMap }}configMap{{ else }}inline{{ end }}
{{- end }}

{{- define "sparkles.backupSource" -}}
{{- if .Values.backup.existingSecret }}secret{{ else if .Values.backup.existingConfigMap }}configMap{{ else }}inline{{ end }}
{{- end }}

{{/* The host names passed as --public-host: the Service's, the Ingress and HTTPRoute hosts and server.publicHosts. */}}
{{- define "sparkles.publicHosts" -}}
{{- $svc := include "sparkles.fullname" . }}
{{- $ns := .Release.Namespace }}
{{- $hosts := list $svc (printf "%s.%s" $svc $ns) (printf "%s.%s.svc" $svc $ns) (printf "%s.%s.svc.%s" $svc $ns .Values.server.clusterDomain) }}
{{- if .Values.ingress.enabled }}
{{- range .Values.ingress.hosts }}
{{- $hosts = append $hosts .host }}
{{- end }}
{{- end }}
{{- if .Values.httpRoute.enabled }}
{{- range .Values.httpRoute.hostnames }}
{{- $hosts = append $hosts . }}
{{- end }}
{{- end }}
{{- range .Values.server.publicHosts }}
{{- $hosts = append $hosts . }}
{{- end }}
{{- toJson (uniq $hosts) }}
{{- end }}

{{/* Whether the server uses a model store volume mounted at models.mountPath. */}}
{{- define "sparkles.modelsVolume" -}}
{{- $d := .Values.models.delivery }}
{{- if or (has $d (list "pull" "image" "imageCopy")) (and (eq $d "download") .Values.models.persistence.enabled) }}true{{ end }}
{{- end }}

{{/* Checks of combinations the schema cannot express. */}}
{{- define "sparkles.validate" -}}
{{- if and (not .Values.auth.enabled) (not .Values.server.allowOpenNetwork) }}
{{- fail "Set auth.enabled with an auth configuration, or server.allowOpenNetwork=true to serve without authentication to every client that reaches the Service" }}
{{- end }}
{{- if and .Values.auth.enabled (not .Values.auth.existingSecret) (not .Values.auth.existingConfigMap) (not .Values.auth.config) }}
{{- fail "auth.enabled needs auth.config, auth.existingSecret or auth.existingConfigMap" }}
{{- end }}
{{- if and .Values.backup.enabled (not .Values.backup.existingSecret) (not .Values.backup.existingConfigMap) (not .Values.backup.config) }}
{{- fail "backup.enabled needs backup.config, backup.existingSecret or backup.existingConfigMap" }}
{{- end }}
{{- if le (int .Values.terminationGracePeriodSeconds) (int (ceil .Values.server.shutdownGrace)) }}
{{- fail "terminationGracePeriodSeconds must be longer than server.shutdownGrace, or Kubernetes kills requests the server is still finishing" }}
{{- end }}
{{- if and (has .Values.models.delivery (list "image" "imageCopy")) (not .Values.models.image.reference) }}
{{- fail "models.delivery image and imageCopy need models.image.reference" }}
{{- end }}
{{- if and (eq .Values.models.delivery "pull") (not .Values.models.manifest.models) }}
{{- fail "models.delivery pull needs models.manifest.models" }}
{{- end }}
{{- if and .Values.ocr.enabled (not .Values.ocr.models.existingClaim) (not .Values.ocr.models.imageReference) }}
{{- fail "ocr.enabled needs ocr.models.existingClaim or ocr.models.imageReference" }}
{{- end }}
{{- end }}
