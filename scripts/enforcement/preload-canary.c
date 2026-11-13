/*
 * Test support for decision 0056's injection check (the amendment of 2026-11-13, "files,
 * descriptors and network"). The Linux enforcement job builds this shared object and names it in
 * LD_PRELOAD for one test unit; it is never part of a release. Its initializer appends the path of
 * the program that loaded it and a newline to the log below. Every error is ignored, so it never
 * breaks the program that loads it; it does nothing else.
 */
#include <fcntl.h>
#include <unistd.h>

#define ARTIFREE_PRELOAD_LOG "/tmp/artifree-enforcement-preload.log"

__attribute__((constructor)) static void artifree_preload_canary(void) {
  char path[4096];
  ssize_t length = readlink("/proc/self/exe", path, sizeof path - 1);
  if (length <= 0) {
    return;
  }
  path[length] = '\n';
  int log = open(ARTIFREE_PRELOAD_LOG, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0644);
  if (log < 0) {
    return;
  }
  ssize_t written = write(log, path, (size_t)length + 1);
  (void)written;
  (void)close(log);
}
