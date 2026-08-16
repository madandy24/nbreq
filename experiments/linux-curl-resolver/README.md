# Linux curl resolver teardown probe

This test-only preload library stalls one named `getaddrinfo` call. The opt-in curl test cancels
that request, shuts its Engine down, measures how long network teardown takes, and verifies both
the fixture's completion marker and the process thread count.

It exists to distinguish immediate canonical `Cancelled` notification from actual resolver and
reactor teardown. It is never linked into NBReq or a product artifact.

On Linux, build it with:

```sh
cc -shared -fPIC -O2 -Wall -Wextra -o stall_getaddrinfo.so stall_getaddrinfo.c -ldl
```

Then run the single opt-in test with `LD_PRELOAD` plus these variables:

- `NBREQ_DNS_STALL_HOST=nbreq-dns-stall.invalid`
- `NBREQ_DNS_STALL_MARKER=/tmp/nbreq-dns-stall`
- `NBREQ_DNS_STALL_MILLISECONDS=1500`
- `NBREQ_DNS_STALL_URL=http://nbreq-dns-stall.invalid/`

The test is skipped unless `NBREQ_DNS_STALL_URL` and `NBREQ_DNS_STALL_MARKER` are both present.
