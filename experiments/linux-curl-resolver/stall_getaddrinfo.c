#define _GNU_SOURCE

#include <dlfcn.h>
#include <fcntl.h>
#include <netdb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

typedef int (*getaddrinfo_fn)(const char *, const char *, const struct addrinfo *,
                             struct addrinfo **);

static void touch_marker(const char *suffix) {
  const char *prefix = getenv("NBREQ_DNS_STALL_MARKER");
  if (prefix == NULL) {
    return;
  }
  char path[1024];
  if (snprintf(path, sizeof(path), "%s.%s", prefix, suffix) >=
      (int)sizeof(path)) {
    return;
  }
  int file = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0600);
  if (file >= 0) {
    close(file);
  }
}

int getaddrinfo(const char *node, const char *service,
                const struct addrinfo *hints, struct addrinfo **result) {
  getaddrinfo_fn real_getaddrinfo =
      (getaddrinfo_fn)dlsym(RTLD_NEXT, "getaddrinfo");
  if (real_getaddrinfo == NULL) {
    return EAI_SYSTEM;
  }

  const char *target = getenv("NBREQ_DNS_STALL_HOST");
  if (node == NULL || target == NULL || strcmp(node, target) != 0) {
    return real_getaddrinfo(node, service, hints, result);
  }

  touch_marker("started");
  const char *delay_text = getenv("NBREQ_DNS_STALL_MILLISECONDS");
  unsigned long delay_ms = delay_text == NULL ? 1500UL : strtoul(delay_text, NULL, 10);
  struct timespec delay = {
      .tv_sec = (time_t)(delay_ms / 1000UL),
      .tv_nsec = (long)((delay_ms % 1000UL) * 1000000UL),
  };
  while (nanosleep(&delay, &delay) != 0) {
  }

  int status = real_getaddrinfo(node, service, hints, result);
  touch_marker("finished");
  return status;
}
