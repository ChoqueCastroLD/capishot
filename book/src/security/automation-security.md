# Automation security

Automation is a privilege boundary because requests can cause filesystem access, application control, UI input, preferences changes, rendering, and command execution.

## Current implementation

- Desktop control binds to `127.0.0.1` and uses one JSON request/reply per line.
- Headless TCP refuses a successfully bound non-loopback address.
- Every desktop-control and headless-TCP connection must authenticate with a 256-bit bearer token before method dispatch.
- Encoded request lines are limited to 1 MiB, active TCP connections to 16, and headless/MCP batches to 256 steps.
- MCP normally uses stdio and can optionally bridge to loopback control TCP.
- The bridge and UI request paths use timeouts.
- Engine commands are expected to reject invalid parameters without panicking.
- Remote filesystem methods use separately granted read and write directory capabilities.
- Automation paths must be relative beneath the applicable root. Absolute paths, parent
  traversal, alternate separators, Windows device names, malformed components, and link escapes
  are rejected before file effects.
- Engine commands that still use ambient paths fail closed at the automation boundary. Synthetic
  UI input reaches the same command policy, and automation-triggered file hooks do not run
  user-configured script-event paths.

## Known limitations

- no per-client or per-tool capabilities;
- no general per-client or per-tool capability model beyond filesystem read/write authority;
- no explicit JSON-depth, response-size, render, document-memory, or command-duration budget;
- one thread per accepted TCP connection;
- no structured security audit event stream;
- full-size renders and expensive engine commands are not charged to a session budget.

## Proposed session model

The transport now establishes an authenticated connection from a cryptographically random credential before method discovery. A future security gateway should turn that authenticated connection into a short-lived, capability-scoped session.

Remaining capabilities should be explicit and composable, for example:

```text
DocumentRead
DocumentWrite
UiInspect
UiControl
FilesystemRead (implemented as a root capability)
FilesystemWrite (implemented as a separate root capability)
PreferencesWrite
ApplicationControl
```

Names and granularity remain a design proposal. The important property is that a client granted document inspection is not implicitly granted arbitrary file writes, pointer injection, preferences changes, or application shutdown.

## Proposed request budgets

The gateway should enforce and test finite values for:

- request bytes and JSON nesting;
- batch steps and nested/recursive calls;
- concurrent connections and per-client in-flight requests;
- render dimensions and returned payload bytes;
- document pixels, decoded bytes, and aggregate session memory;
- command duration or cancellation where the engine supports it.

Limits should fail with stable errors, be applied before expensive work, and be configurable only inside safe global ceilings. Audit events should capture session identity, capability decision, method/command ID, outcome, duration, and redacted path handle—not file contents, tokens, or secrets.

## Deployment guidance today

Prefer stdio, start control TCP only when needed, grant the narrowest read and write roots, use a
private token file, keep the listener on loopback, use a least-privileged OS account, and isolate
untrusted agents from sensitive files. Do not commit or log bearer tokens. Do not tunnel or proxy
the unencrypted loopback protocol to another host.
