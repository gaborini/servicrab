# servicrab-protocol

The wire types spoken by the [servicrab](https://github.com/gaborini/servicrab)
daemon: newline-delimited JSON over a Unix socket, one request per line, one
response per line.

```json
{"type":"status"}
{"type":"status","services":[{"name":"api","state":"running","pid":5512,"restarts":0,"health":"healthy"}]}
```

The crate depends on neither the runtime nor Tokio, so any client can link it
to drive a daemon, follow its event stream, or generate bindings from it.

Dual-licensed under MIT OR Apache-2.0.
