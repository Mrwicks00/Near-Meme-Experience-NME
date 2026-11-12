`wasi-http-client` 0.2.1 (Apache-2.0 OR MIT, wacker-dev/wasi-http-client) with one change in `src/request.rs`: the request body is
written in chunks of at most 4096 bytes. The upstream single `blocking_write_and_flush(&body)` traps for larger bodies.
