// A test-only LD_PRELOAD shim for the durability tests of the write-ahead log (spec X09
// section 10). It sits between a store and libc and does two things with the files under
// $SPARKLES_IO_ROOT.
//
//  * Recording. With $SPARKLES_IO_LOG set it appends every write, truncation and
//    synchronization to that file, in the order the process issued them, with the bytes
//    written. tests/power_loss.rs replays the log to rebuild the files as a power loss at
//    any point would leave them.
//  * Faults. $SPARKLES_IO_FAULT is a comma-separated list of OP:SUFFIX:N or OP:SUFFIX:N+
//    entries. OP is fdatasync, fsync or pwrite (also for write). The call number N (counted
//    from 1 over the calls on files whose path ends in SUFFIX) fails with EIO instead of
//    reaching the kernel, and with N+ so does every later call too.
//
// Log record, little endian:
//   u8 kind (1 write begin, 2 write end, 3 truncate, 4 sync begin, 5 sync end),
//   u8 flag (write: 1 when the descriptor is O_DSYNC; end records: 1 when the call failed),
//   u16 path length, path bytes, u64 offset, u64 length, then for a write the bytes.
// Only the shim's own bookkeeping is serialized: the real calls run concurrently.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

enum { K_WRITE = 1, K_WRITE_END = 2, K_TRUNC = 3, K_SYNC = 4, K_SYNC_END = 5 };

static int (*real_fdatasync)(int);
static int (*real_fsync)(int);
static ssize_t (*real_pwrite)(int, const void *, size_t, off_t);
static ssize_t (*real_write)(int, const void *, size_t);
static int (*real_ftruncate)(int, off_t);
static int (*real_open)(const char *, int, ...);
static off_t (*real_lseek)(int, off_t, int);

static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static int logfd = -1;
static char root[PATH_MAX];
static size_t root_len;
static int ready;
static long delay_us;

#define MAX_FAULTS 16
static struct {
  char op[16];
  char suffix[64];
  long n;
  int from_n;
  long seen;
} faults[MAX_FAULTS];
static int nfaults;

static void init(void) {
  if (ready) return;
  real_fdatasync = dlsym(RTLD_NEXT, "fdatasync");
  real_fsync = dlsym(RTLD_NEXT, "fsync");
  real_pwrite = dlsym(RTLD_NEXT, "pwrite64");
  real_write = dlsym(RTLD_NEXT, "write");
  real_ftruncate = dlsym(RTLD_NEXT, "ftruncate64");
  real_open = dlsym(RTLD_NEXT, "open");
  real_lseek = dlsym(RTLD_NEXT, "lseek64");
  const char *r = getenv("SPARKLES_IO_ROOT");
  if (r) {
    char *real = realpath(r, root);
    if (!real) strncpy(root, r, sizeof root - 1);
    root_len = strlen(root);
  }
  const char *d = getenv("SPARKLES_IO_DELAY_US");
  if (d) delay_us = atol(d);
  const char *log = getenv("SPARKLES_IO_LOG");
  if (log && root_len) logfd = real_open(log, O_WRONLY | O_CREAT | O_TRUNC | O_APPEND, 0644);
  const char *spec = getenv("SPARKLES_IO_FAULT");
  if (spec && root_len) {
    char *copy = strdup(spec), *save = NULL;
    for (char *e = strtok_r(copy, ",", &save); e && nfaults < MAX_FAULTS; e = strtok_r(NULL, ",", &save)) {
      char op[16], suffix[64];
      long n;
      char plus = 0;
      if (sscanf(e, "%15[^:]:%63[^:]:%ld%c", op, suffix, &n, &plus) >= 3) {
        strcpy(faults[nfaults].op, op);
        strcpy(faults[nfaults].suffix, suffix);
        faults[nfaults].n = n;
        faults[nfaults].from_n = plus == '+';
        nfaults++;
      }
    }
    free(copy);
  }
  ready = 1;
}

// The path of `fd` when it is a file under the root.
static int path_of(int fd, char *buf, size_t cap) {
  if (root_len == 0) return 0;
  char link[64];
  snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
  ssize_t n = readlink(link, buf, cap - 1);
  if (n <= 0) return 0;
  buf[n] = 0;
  return (size_t)n > root_len && strncmp(buf, root, root_len) == 0 && buf[root_len] == '/';
}

static int ends_with(const char *s, const char *suffix) {
  size_t a = strlen(s), b = strlen(suffix);
  return a >= b && strcmp(s + a - b, suffix) == 0;
}

static int should_fail(const char *op, const char *path) {
  int fail = 0;
  // faults count the calls made once $SPARKLES_IO_ARM exists
  const char *arm = getenv("SPARKLES_IO_ARM");
  if (arm && access(arm, F_OK) != 0) return 0;
  pthread_mutex_lock(&mu);
  for (int i = 0; i < nfaults; i++) {
    if (strcmp(faults[i].op, op) != 0 || !ends_with(path, faults[i].suffix)) continue;
    long seen = ++faults[i].seen;
    if (seen == faults[i].n || (faults[i].from_n && seen > faults[i].n)) fail = 1;
  }
  pthread_mutex_unlock(&mu);
  return fail;
}

static void record(uint8_t kind, uint8_t flag, const char *path, uint64_t off, uint64_t len, const void *data) {
  if (logfd < 0) return;
  uint16_t pl = (uint16_t)strlen(path);
  size_t datalen = data ? (size_t)len : 0;
  size_t total = 2 + 2 + pl + 8 + 8 + datalen;
  char *b = malloc(total);
  size_t p = 0;
  b[p++] = (char)kind;
  b[p++] = (char)flag;
  memcpy(b + p, &pl, 2), p += 2;
  memcpy(b + p, path, pl), p += pl;
  memcpy(b + p, &off, 8), p += 8;
  memcpy(b + p, &len, 8), p += 8;
  if (datalen) memcpy(b + p, data, datalen);
  pthread_mutex_lock(&mu);
  size_t done = 0;
  while (done < total) {
    ssize_t w = real_write(logfd, b + done, total - done);
    if (w <= 0) break;
    done += (size_t)w;
  }
  pthread_mutex_unlock(&mu);
  free(b);
}

static int sync_call(const char *op, int fd) {
  init();
  int (*real)(int) = strcmp(op, "fdatasync") == 0 ? real_fdatasync : real_fsync;
  char path[PATH_MAX];
  if (!path_of(fd, path, sizeof path)) return real(fd);
  record(K_SYNC, 0, path, 0, 0, NULL);
  int r;
  if (should_fail(op, path)) {
    errno = EIO;
    r = -1;
  } else {
    r = real(fd);
    if (delay_us) usleep((useconds_t)delay_us);
  }
  int saved = errno;
  record(K_SYNC_END, r != 0, path, 0, 0, NULL);
  errno = saved;
  return r;
}

int fdatasync(int fd) { return sync_call("fdatasync", fd); }
int fsync(int fd) { return sync_call("fsync", fd); }

static ssize_t write_call(int fd, const void *buf, size_t n, off_t off, int positioned) {
  init();
  char path[PATH_MAX];
  if (!path_of(fd, path, sizeof path)) return positioned ? real_pwrite(fd, buf, n, off) : real_write(fd, buf, n);
  int dsync = (fcntl(fd, F_GETFL) & O_DSYNC) == O_DSYNC;
  if (!positioned) {
    int fl = fcntl(fd, F_GETFL);
    if (fl & O_APPEND) {
      struct stat st;
      off = fstat(fd, &st) == 0 ? st.st_size : 0;
    } else {
      off = real_lseek(fd, 0, SEEK_CUR);
    }
  }
  record(K_WRITE, dsync, path, (uint64_t)off, n, buf);
  ssize_t r;
  if (should_fail("pwrite", path)) {
    errno = EIO;
    r = -1;
  } else {
    r = positioned ? real_pwrite(fd, buf, n, off) : real_write(fd, buf, n);
    if (dsync && delay_us) usleep((useconds_t)delay_us);
  }
  int saved = errno;
  record(K_WRITE_END, r != (ssize_t)n, path, (uint64_t)off, r < 0 ? 0 : (uint64_t)r, NULL);
  errno = saved;
  return r;
}

ssize_t pwrite64(int fd, const void *buf, size_t n, off_t off) { return write_call(fd, buf, n, off, 1); }
ssize_t pwrite(int fd, const void *buf, size_t n, off_t off) { return write_call(fd, buf, n, off, 1); }
ssize_t write(int fd, const void *buf, size_t n) {
  if (!ready) init();
  return write_call(fd, buf, n, 0, 0);
}

int ftruncate64(int fd, off_t len) {
  init();
  char path[PATH_MAX];
  if (path_of(fd, path, sizeof path)) record(K_TRUNC, 0, path, 0, (uint64_t)len, NULL);
  return real_ftruncate(fd, len);
}
int ftruncate(int fd, off_t len) { return ftruncate64(fd, len); }
