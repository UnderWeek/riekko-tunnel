<div align="center">

# ❄️ Riekko Tunnel

### A small, focused tunnel client for private networking.

**Simple to start. Clear when something breaks. Quiet when everything works.**

<br>

[English](README.md) · [Русский](README_RU.md)

</div>

---

## 🌨️ Overview

**Riekko Tunnel** is a client for creating and managing private network tunnels without turning the process into a maze of settings.

The project is built around a simple idea:

```text
config → validate → connect → route → stay out of the way
```

Riekko is not trying to become a giant networking control panel.

It is meant to provide a clean layer around the tunnel itself: configuration, connection state, useful diagnostics and a straightforward user experience.

---

## 🧊 What Riekko cares about

<table>
<tr>
<td width="50%">

### ⚡ Fast path

Common actions should stay short.

No unnecessary setup flow just to establish a connection.

</td>
<td width="50%">

### 🔍 Clear state

The client should always make it obvious whether the tunnel is idle, starting, connected or broken.

</td>
</tr>
<tr>
<td width="50%">

### 🪶 Small surface

Features should exist because they solve an actual problem, not because another VPN client has them.

</td>
<td width="50%">

### 🔐 Private by default

A tunneling client has no reason to collect unrelated personal data.

</td>
</tr>
</table>

---

## 🛰️ How it fits together

```text
                 ┌─────────────────────┐
                 │       Riekko        │
                 │       Client        │
                 └──────────┬──────────┘
                            │
                   validated config
                            │
                            ▼
                 ┌─────────────────────┐
                 │    Tunnel layer     │
                 │ transport / routes  │
                 └──────────┬──────────┘
                            │
                            ▼
                 ┌─────────────────────┐
                 │       Network       │
                 └─────────────────────┘
```

The client handles the parts a user should not have to manually babysit every time:

- configuration loading
- validation
- tunnel start and stop
- connection state
- basic diagnostics
- recovery from common failures

The exact backend and supported transports will be documented as the implementation stabilizes.

---

## 🚦 Connection states

Riekko keeps its state model intentionally small.

| State | Description |
|---|---|
| `IDLE` | No tunnel is active |
| `STARTING` | Configuration is being checked and the tunnel is starting |
| `CONNECTED` | The tunnel is active and routing traffic |
| `RECONNECTING` | Riekko is attempting to recover the connection |
| `ERROR` | The connection failed and needs attention |

A status screen should answer the important questions immediately:

```text
RIEKKO

● CONNECTED

Endpoint    nl.example.net
Latency     31 ms
Uptime      01:18:42

↑ 14.8 MB
↓ 92.1 MB
```

No animated shield required.

---

## 🧩 Interface direction

The interface is planned around a few focused areas instead of dozens of nested settings pages.

<table>
<thead>
<tr>
<th>Area</th>
<th>Purpose</th>
</tr>
</thead>
<tbody>
<tr>
<td>🌐 <strong>Connection</strong></td>
<td>Current tunnel, endpoint, state and quick controls</td>
</tr>
<tr>
<td>🗂️ <strong>Profiles</strong></td>
<td>Saved and imported tunnel configurations</td>
</tr>
<tr>
<td>📈 <strong>Session</strong></td>
<td>Basic traffic and connection information</td>
</tr>
<tr>
<td>⚙️ <strong>Preferences</strong></td>
<td>Local application behavior</td>
</tr>
</tbody>
</table>

The goal is not to hide advanced configuration.

The goal is to keep it **out of the main path until it is actually needed**.

---

## 🧭 Configuration philosophy

Riekko should prefer configuration that is:

- readable
- portable
- explicit
- easy to validate
- easy to debug

Instead of burying everything in application state, a tunnel profile should remain understandable on its own.

```text
profile
├── endpoint
├── transport
├── authentication
├── routing
└── optional overrides
```

The final schema is not stable yet.

> [!IMPORTANT]
> Configuration examples in early builds may change before the first stable release.

---

## 🛠️ Repository direction

The project is expected to keep networking logic separate from the user-facing client.

```text
riekko-tunnel/
│
├── core/          tunnel lifecycle and networking logic
├── client/        application state and UI integration
├── platform/      OS-specific networking code
├── configs/       examples and test profiles
├── tests/         integration and behavior tests
└── docs/          technical documentation
```

This keeps the core easier to test and avoids tying tunnel behavior directly to one interface.

---

## 🧪 Current status

<div align="center">

### **Early development**

The architecture is still moving and compatibility is not guaranteed yet.

</div>

Things that may change:

- configuration format
- repository layout
- UI structure
- backend integration
- supported platforms
- command-line interface

That is expected until the first stable release.

---

## 🗺️ Roadmap

### Foundation

- [ ] Define tunnel lifecycle
- [ ] Define configuration schema
- [ ] Add configuration validation
- [ ] Add structured error reporting
- [ ] Add logging and diagnostics

### Client

- [ ] Connection overview
- [ ] Profile management
- [ ] Import / export
- [ ] Reconnect controls
- [ ] Basic session statistics

### Platform work

- [ ] Windows integration
- [ ] Linux integration
- [ ] macOS integration
- [ ] Evaluate Android support

### Later

- [ ] Stable configuration format
- [ ] Automated tests for common network failures
- [ ] Release packaging
- [ ] Documentation
- [ ] First stable release

---

## 🔧 Development

Build instructions will be added once the initial implementation and toolchain are fixed.

For now, the repository can be cloned normally:

```bash
git clone https://github.com/<owner>/riekko-tunnel.git
cd riekko-tunnel
```

> [!TIP]
> If you are working on the tunnel core, keep platform-specific behavior isolated whenever possible.

---

## 🤝 Contributing

Riekko is still small enough that architectural decisions matter.

Issues and pull requests are welcome, especially when they make the project:

- easier to understand
- easier to test
- easier to recover
- less surprising
- smaller without losing capability

For large changes, opening an issue before writing the implementation is preferred.

---

## 📜 License

See [`LICENSE`](LICENSE).

Third-party components, when introduced, remain covered by their own licenses.

---

<div align="center">

## ❄️ Riekko

**Connect quietly. Stay in control.**

</div>
