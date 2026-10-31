// A test-only LD_PRELOAD shim for the durability tests of the write-ahead log (spec X09
// section 10). It sits between a store and libc and does two things with the files under
// $SPARKLES_IO_ROOT.
//
//  * Recording. With $SPARKLES_IO_LOG set it appends every write, truncation and
//    synchronization to that file, in the order the process issued them, with the bytes
//    written, and every directory operation that succeeded: the creation of a file, a
//    rename and an unlink. A synchronization of a directory is recorded like one of a
//    file. tests/power_loss.rs replays the log to rebuild the files as a power loss at any
//    point would leave them.
//  * Faults. $SPARKLES_IO_FAULT is a comma-separated list of OP:SUFFIX:N or OP:SUFFIX:N+
//    entries. OP is fdatasync, fsync, sync_file_range or pwrite (also for write). The call
//    number N (counted
//    from 1 over the calls on files whose path ends in SUFFIX) fails with EIO instead of
//    reaching the kernel, and with N+ so does every later call too.
//
// With SPARKLES_IO_SYNC_ENTERED and SPARKLES_IO_SYNC_RELEASE set, the first armed
// text-file sync signals entry and waits up to five seconds for a designated writer's
// release. This deterministic crash-fixture interleaving is not a performance mode.
//
// Log record, little endian:
//   u8 kind (1 write begin, 2 write end, 3 truncate, 4 sync begin, 5 sync end, 6 rename,
//   7 unlink, 8 create, 9 write-back begin, 10 write-back end),
//   u8 flag (write: 1 when the descriptor is O_DSYNC; write-back begin: 1 when the call
//   waits for the writes to complete; end records: 1 when the call failed),
//   u16 path length, path bytes, u64 offset, u64 length, then for a write the bytes and for
//   a rename the new path (the path is the old one).
// Only the shim's own bookkeeping is serialized: the real calls run concurrently. A
// process that exits while another thread appends a record can leave that last record
// cut short, and the reader drops it.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <libgen.h>
#include <limits.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

enum {
  K_WRITE = 1,
  K_WRITE_END = 2,
  K_TRUNC = 3,
  K_SYNC = 4,
  K_SYNC_END = 5,
  K_RENAME = 6,
  K_UNLINK = 7,
  K_CREATE = 8,
  K_WRITEBACK = 9,
  K_WRITEBACK_END = 10
};

static int (*real_fdatasync)(int);
static int (*real_fsync)(int);
static int (*real_sync_file_range)(int, off_t, off_t, unsigned int);
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
static const char *sync_entered, *sync_release;
static int sync_gated;

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
  real_sync_file_range = dlsym(RTLD_NEXT, "sync_file_range");
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
  sync_entered = getenv("SPARKLES_IO_SYNC_ENTERED");
  sync_release = getenv("SPARKLES_IO_SYNC_RELEASE");
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

static int gate_text_sync(const char *path) {
  const char *arm = getenv("SPARKLES_IO_ARM");
  if (!sync_entered || !sync_release || !strstr(path, "/text/") ||
      (arm && access(arm, F_OK) != 0)) return 0;
  pthread_mutex_lock(&mu);
  int gate = !sync_gated;
  sync_gated = 1;
  pthread_mutex_unlock(&mu);
  if (!gate) return 0;
  int fd = real_open(sync_entered, O_WRONLY | O_CREAT | O_TRUNC, 0644);
  if (fd < 0) return -1;
  ssize_t written = real_write(fd, "1", 1);
  int saved = errno;
  close(fd);
  if (written != 1) { errno = saved; return -1; }
  struct timespec start, now;
  if (clock_gettime(CLOCK_MONOTONIC, &start) != 0) return -1;
  while (access(sync_release, F_OK) != 0) {
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return -1;
    if ((now.tv_sec - start.tv_sec) * 1000000000LL + now.tv_nsec - start.tv_nsec >= 5000000000LL) {
      errno = ETIMEDOUT;
      return -1;
    }
    usleep(1000);
  }
  return 0;
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
    if (r == 0 && gate_text_sync(path) != 0) r = -1;
  }
  int saved = errno;
  record(K_SYNC_END, r != 0, path, 0, 0, NULL);
  errno = saved;
  return r;
}

int fdatasync(int fd) { return sync_call("fdatasync", fd); }
int fsync(int fd) { return sync_call("fsync", fd); }

// A write-back of a range (length 0: to the end of the file). One that waits for the
// writes to complete (SYNC_FILE_RANGE_WAIT_AFTER) leaves them in the device's volatile
// cache, and the next synchronization of any file, whose cache flush covers every write
// completed before it, makes them durable. tests/power_loss.rs models it so.
int sync_file_range(int fd, off_t off, off_t len, unsigned int flags) {
  init();
  char path[PATH_MAX];
  if (!path_of(fd, path, sizeof path)) return real_sync_file_range(fd, off, len, flags);
  int wait = (flags & SYNC_FILE_RANGE_WAIT_AFTER) != 0;
  record(K_WRITEBACK, (uint8_t)wait, path, (uint64_t)off, (uint64_t)len, NULL);
  int r;
  if (should_fail("sync_file_range", path)) {
    errno = EIO;
    r = -1;
  } else {
    r = real_sync_file_range(fd, off, len, flags);
  }
  int saved = errno;
  record(K_WRITEBACK_END, r != 0, path, (uint64_t)off, (uint64_t)len, NULL);
  errno = saved;
  return r;
}

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

// ------------------------------------------------------------- directory operations

static int (*real_rename)(const char *, const char *);
static int (*real_renameat)(int, const char *, int, const char *);
static int (*real_unlink)(const char *);
static int (*real_unlinkat)(int, const char *, int);
static int (*real_open64)(const char *, int, ...);
static int (*real_openat)(int, const char *, int, ...);
static int (*real_openat64)(int, const char *, int, ...);
static pthread_once_t dir_once = PTHREAD_ONCE_INIT;

static void dir_init(void) {
  real_rename = dlsym(RTLD_NEXT, "rename");
  real_renameat = dlsym(RTLD_NEXT, "renameat");
  real_unlink = dlsym(RTLD_NEXT, "unlink");
  real_unlinkat = dlsym(RTLD_NEXT, "unlinkat");
  real_open64 = dlsym(RTLD_NEXT, "open64");
  real_openat = dlsym(RTLD_NEXT, "openat");
  real_openat64 = dlsym(RTLD_NEXT, "openat64");
}

// The absolute path of `path`, taken relative to `dirfd`, with its directory resolved,
// when it is under the root. The entry itself need not exist.
static int resolve(int dirfd, const char *path, char *out, size_t cap) {
  if (root_len == 0 || !path) return 0;
  char full[PATH_MAX];
  if (path[0] == '/') {
    snprintf(full, sizeof full, "%s", path);
  } else if (dirfd == AT_FDCWD) {
    char cwd[PATH_MAX];
    if (!getcwd(cwd, sizeof cwd)) return 0;
    snprintf(full, sizeof full, "%s/%s", cwd, path);
  } else {
    char link[64], dir[PATH_MAX];
    snprintf(link, sizeof link, "/proc/self/fd/%d", dirfd);
    ssize_t n = readlink(link, dir, sizeof dir - 1);
    if (n <= 0) return 0;
    dir[n] = 0;
    snprintf(full, sizeof full, "%s/%s", dir, path);
  }
  char a[PATH_MAX], b[PATH_MAX], real[PATH_MAX];
  strcpy(a, full);
  strcpy(b, full);
  if (!realpath(dirname(a), real)) return 0;
  snprintf(out, cap, "%s/%s", real, basename(b));
  size_t n = strlen(out);
  return n > root_len && strncmp(out, root, root_len) == 0 && out[root_len] == '/';
}

static int rename_call(int olddirfd, const char *from, int newdirfd, const char *to, int at) {
  init();
  pthread_once(&dir_once, dir_init);
  char a[PATH_MAX], b[PATH_MAX];
  int ia = resolve(olddirfd, from, a, sizeof a);
  int ib = resolve(newdirfd, to, b, sizeof b);
  int r = at ? real_renameat(olddirfd, from, newdirfd, to) : real_rename(from, to);
  int saved = errno;
  if (r == 0 && ia && ib) record(K_RENAME, 0, a, 0, strlen(b), b);
  errno = saved;
  return r;
}

int rename(const char *from, const char *to) { return rename_call(AT_FDCWD, from, AT_FDCWD, to, 0); }
int renameat(int olddirfd, const char *from, int newdirfd, const char *to) {
  return rename_call(olddirfd, from, newdirfd, to, 1);
}

static int unlink_call(int dirfd, const char *path, int flags, int at) {
  init();
  pthread_once(&dir_once, dir_init);
  char p[PATH_MAX];
  int in = resolve(dirfd, path, p, sizeof p);
  int r = at ? real_unlinkat(dirfd, path, flags) : real_unlink(path);
  int saved = errno;
  if (r == 0 && in) record(K_UNLINK, 0, p, 0, 0, NULL);
  errno = saved;
  return r;
}

int unlink(const char *path) { return unlink_call(AT_FDCWD, path, 0, 0); }
int unlinkat(int dirfd, const char *path, int flags) { return unlink_call(dirfd, path, flags, 1); }

// An open that creates the file (O_CREAT) or empties it (O_TRUNC) records that.
static int open_call(int dirfd, const char *path, int flags, mode_t mode, int which) {
  init();
  pthread_once(&dir_once, dir_init);
  char p[PATH_MAX];
  int in = (flags & (O_CREAT | O_TRUNC)) && (flags & O_TMPFILE) != O_TMPFILE &&
           resolve(dirfd, path, p, sizeof p);
  int existed = in && access(p, F_OK) == 0;
  int fd;
  switch (which) {
    case 0: fd = real_open(path, flags, mode); break;
    case 1: fd = real_open64(path, flags, mode); break;
    case 2: fd = real_openat(dirfd, path, flags, mode); break;
    default: fd = real_openat64(dirfd, path, flags, mode); break;
  }
  int saved = errno;
  if (fd >= 0 && in) {
    if (!existed && (flags & O_CREAT)) {
      record(K_CREATE, 0, p, 0, 0, NULL);
    } else if (existed && (flags & O_TRUNC) && (flags & O_ACCMODE) != O_RDONLY) {
      record(K_TRUNC, 0, p, 0, 0, NULL);
    }
  }
  errno = saved;
  return fd;
}

#define MODE_OF(flags)                                                                   \
  mode_t mode = 0;                                                                       \
  if ((flags & O_CREAT) || (flags & O_TMPFILE) == O_TMPFILE) {                           \
    va_list ap;                                                                          \
    va_start(ap, flags);                                                                 \
    mode = (mode_t)va_arg(ap, int);                                                      \
    va_end(ap);                                                                          \
  }

int open(const char *path, int flags, ...) {
  MODE_OF(flags);
  return open_call(AT_FDCWD, path, flags, mode, 0);
}
int open64(const char *path, int flags, ...) {
  MODE_OF(flags);
  return open_call(AT_FDCWD, path, flags, mode, 1);
}
int openat(int dirfd, const char *path, int flags, ...) {
  MODE_OF(flags);
  return open_call(dirfd, path, flags, mode, 2);
}
int openat64(int dirfd, const char *path, int flags, ...) {
  MODE_OF(flags);
  return open_call(dirfd, path, flags, mode, 3);
}
