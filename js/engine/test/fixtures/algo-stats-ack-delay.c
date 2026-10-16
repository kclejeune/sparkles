/* Private Linux test interposer. Loaded only into one bounded owned child. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static ssize_t (*next_write)(int, const void *, size_t);
static int (*next_close)(int);
static int (*next_rename)(const char *, const char *);
static __thread int intercepted[2];
static __thread int closed_once;

static int ack(const char *path) {
  const char *root = getenv("SPARKLES_ACK_ROOT");
  const char *name = getenv("SPARKLES_ACK_NAME");
  if (!root || !name || strncmp(path, root, strlen(root)) || path[strlen(root)] != '/') return 0;
  const char *base = path + strlen(root) + 1;
  if (strchr(base, '/')) return 0;
  char final[40], temp[40];
  snprintf(final, sizeof(final), "%s.json", name);
  snprintf(temp, sizeof(temp), ".%s.json.tmp", name);
  return !strcmp(base, final) ? 1 : (!strcmp(base, temp) ? 2 : 0);
}
static int fd_ack(int fd, char *path, size_t size) {
  char link[64];
  snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
  ssize_t n = readlink(link, path, size - 1);
  if (n < 0) return 0;
  path[n] = 0;
  return ack(path);
}
static void record(const char *event, const char *path, ssize_t result) {
  const char *root = getenv("SPARKLES_ACK_ROOT");
  char log[4096], row[8192];
  snprintf(log, sizeof(log), "%s/intercept.jsonl", root);
  int fd = open(log, O_WRONLY | O_APPEND | O_CREAT, 0600);
  if (fd < 0) _exit(122);
  int n = snprintf(row, sizeof(row), "{\"event\":\"%s\",\"path\":\"%s\",\"result\":%zd}\n", event, path, result);
  if (next_write(fd, row, n) != n) _exit(123);
  if (!next_close) next_close = dlsym(RTLD_NEXT, "close");
  next_close(fd);
}
ssize_t write(int fd, const void *buf, size_t size) {
  if (!next_write) next_write = dlsym(RTLD_NEXT, "write");
  char path[4096];
  int kind = fd_ack(fd, path, sizeof(path));
  const char *mode = getenv("SPARKLES_ACK_MODE");
  if (kind && mode && !intercepted[kind - 1]++) {
    char attempted[4096];
    snprintf(attempted, sizeof(attempted), "%s/attempted-%s.json", getenv("SPARKLES_ACK_ROOT"), getenv("SPARKLES_ACK_NAME"));
    int captured = open(attempted, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (captured < 0 || next_write(captured, buf, size) != (ssize_t)size) _exit(124);
    if (!next_close) next_close = dlsym(RTLD_NEXT, "close");
    next_close(captured);
    if (!strcmp(mode, "fail-write")) { record("write-failed", path, -1); errno = EIO; return -1; }
    if (!strcmp(mode, "partial") && size > 1) {
      ssize_t n = next_write(fd, buf, size / 2);
      record("prefix-written", path, n);
      usleep(100000);
      return n; /* Forward actual progress; write_all must write the remainder. */
    }
    if (!strcmp(mode, "delay")) { record("before-bytes", path, 0); usleep(100000); }
  }
  ssize_t n = next_write(fd, buf, size);
  if (kind && mode) record("write-returned", path, n);
  return n;
}
int close(int fd) {
  if (!next_close) next_close = dlsym(RTLD_NEXT, "close");
  if (!next_write) next_write = dlsym(RTLD_NEXT, "write");
  char path[4096]; int kind = fd_ack(fd, path, sizeof(path));
  const char *mode = getenv("SPARKLES_ACK_MODE");
  int result = next_close(fd);
  if (kind && mode && !strcmp(mode, "fail-close") && !closed_once++) {
    record("close-failed", path, result); errno = EIO; return -1;
  }
  return result;
}
int rename(const char *from, const char *to) {
  if (!next_rename) next_rename = dlsym(RTLD_NEXT, "rename");
  if (!next_write) next_write = dlsym(RTLD_NEXT, "write");
  const char *mode = getenv("SPARKLES_ACK_MODE");
  if (ack(to) && mode && !strcmp(mode, "fail-rename")) {
    record("rename-failed", to, -1); errno = EIO; return -1;
  }
  return next_rename(from, to);
}
