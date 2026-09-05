<div align="center">

# ❄️ Riekko Tunnel

### Private networking without unnecessary complexity.

A focused tunneling client built around speed, clarity and predictable behavior.

**English** · [Русский](README_RU.md)

</div>

---

## ✨ What is Riekko?

**Riekko Tunnel** is an independent networking project for creating and managing private tunnels.

The idea is simple: keep the everyday workflow clear, keep advanced behavior accessible, and avoid turning basic network configuration into a wall of switches.

```text
┌──────────────────────┐
│        Riekko        │
│       Client UI      │
└──────────┬───────────┘
           │
           │ configuration
           ▼
┌──────────────────────┐
│    Tunnel Engine     │
│ routing / transport  │
└──────────┬───────────┘
           │
           ▼
        Internet
```

Riekko is designed to stay out of the way once a connection is established.

## 🎯 Project goals

* ⚡ **Fast connection flow** without unnecessary setup steps
* 🪶 **Lightweight client** with a small and understandable core
* 🔐 **Private by design** with no reason to collect unrelated user data
* 🧩 **Readable configuration** instead of hidden application magic
* 🛠️ **Useful diagnostics** when a connection fails
* 🔄 **Reliable state handling** between configuration, connection and UI
* 🧱 A structure that can grow without turning into a collection of unrelated features

## 🚇 Tunnel workflow

Riekko keeps the normal connection path intentionally straightforward.

```text
Configuration
     │
     ▼
Validation
     │
     ▼
Tunnel startup
     │
     ▼
Route traffic
     │
     ▼
Connected
```

The client should make the important state visible without exposing every internal detail at once.

| State             | Meaning                                                     |
| ----------------- | ----------------------------------------------------------- |
| ⚪ **Idle**        | No active tunnel                                            |
| 🟡 **Connecting** | Configuration is being validated and the tunnel is starting |
| 🟢 **Connected**  | Traffic is routed through the active tunnel                 |
| 🔴 **Error**      | Startup or connection failed with a readable reason         |

## 🧭 Configuration

Riekko aims to keep configuration explicit and portable.

A configuration should describe **what should happen**, while the application handles the repetitive parts around starting, stopping and monitoring the tunnel.

```text
┌─────────────────────────────────┐
│ Riekko configuration            │
│                                 │
│ Server      example.net         │
│ Transport   configured          │
│ Routing     enabled             │
│                                 │
│          [ Connect ]            │
└─────────────────────────────────┘
```

The exact configuration format is still evolving and may change before the first stable release.

## 🧩 Planned interface

The project is expected to stay compact rather than grow into a giant networking dashboard.

| Page            | Purpose                                             |
| --------------- | --------------------------------------------------- |
| 🏠 **Overview** | Current connection, endpoint and tunnel state       |
| 🚇 **Tunnels**  | Create, import and manage tunnel configurations     |
| 📊 **Traffic**  | Basic session statistics and connection information |
| ⚙️ **Settings** | Application behavior and local preferences          |

## 🗺️ Roadmap

* [ ] Define the core tunnel architecture
* [ ] Add configuration loading and validation
* [ ] Implement tunnel lifecycle management
* [ ] Add connection status and readable errors
* [ ] Add import and export for configurations
* [ ] Build the first desktop interface
* [ ] Add basic traffic statistics
* [ ] Add reconnect and recovery behavior
* [ ] Add platform-specific networking integration
* [ ] Publish the first usable release
* [ ] Reach the legendary networking milestone: **it just works**

## 💻 Platforms

Platform support will depend on the networking backend and the maturity of the project.

| Platform | Status              |
| -------- | ------------------- |
| Windows  | Planned             |
| Linux    | Planned             |
| macOS    | Planned             |
| Android  | Under consideration |

The first stable target will be documented once the core implementation is ready.

## 🔧 Development

Riekko is currently in early development.

The repository is expected to keep the networking core separate from platform-specific interfaces so that the tunnel logic can remain testable and reusable.

```text
riekko-tunnel/
├── core/          # tunnel and networking logic
├── client/        # application layer
├── platform/      # OS-specific integration
├── configs/       # examples and test configurations
└── docs/          # documentation
```

Development commands and build instructions will be added once the initial project structure is finalized.

```bash
git clone https://github.com/<your-name>/riekko-tunnel.git
cd riekko-tunnel
```

## 🐦 Why Riekko?

**Riekko** is the Finnish name for the willow ptarmigan — a northern bird whose winter plumage blends almost completely into the snow.

That fits the project surprisingly well.

Riekko should be visible when you need to configure it, understandable when something goes wrong, and otherwise quiet enough to forget about.

> **Connect. Route. Disappear into the background.**

## 🤝 Contributing

Riekko is still taking shape, so architecture discussions, bug reports and focused pull requests are welcome.

Good contributions should make the project:

* simpler to understand
* easier to debug
* more reliable
* easier to maintain
* less surprising

Large changes should preferably start as an issue before implementation.

## 📜 License

See [`LICENSE`](LICENSE).

If third-party networking components are added, they remain covered by their respective licenses.

---

<div align="center">

### ❄️ Riekko

**Quiet connection. Clear control.**

</div>
