# Cordyceps

> **An autonomous AI agent you run, control remotely, and watch improve itself.**

Cordyceps is a self-hosted, always-on AI agent. You stand it up on a machine — a server, a homelab box, a VM — and it runs continuously, taking direction from you remotely and carrying out tasks on your behalf. Over time it gets better at its job by extending *itself*: writing new skills, adding tools, and making direct changes to its own codebase.

Think of it as a long-lived operator that lives on your infrastructure. You don't restart it for every task. You give it goals, check in when you want, and it keeps working — and keeps growing new capabilities — in the background.

---

## What It Does

- **Runs autonomously on a machine you own.** Cordyceps is a persistent process, not a one-shot CLI invocation. It maintains state across tasks and sessions and stays available between requests.
- **Takes direction remotely.** Queue tasks, change priorities, and inspect what it's doing from wherever you are — no need to be at the host.
- **Executes real work.** It plans, acts through tools, verifies its results, and reports back. Long-running and recurring tasks are first-class.
- **Self-improves.** When it hits a capability it doesn't have, it can add one — authoring a new skill, building a new tool, or modifying its own source — then bringing the new capability online without a full redeploy.

In spirit it sits alongside projects like OpenClaw and the Hermes-style autonomous agents: a single capable agent given the autonomy and the channels to keep working on your behalf.

---

## Where It Fits

Cordyceps is the third piece of a layered ecosystem. Each layer builds on the one below it.

| Project | Role |
|---|---|
| **[spore-core](../spore-core)** | The agentic harness runtime — the loop, tools, sandbox, memory, sensors, and the improvement flywheel. The foundation every agent stands on. `Agent = Model + Harness`. |
| **[spore](../spore)** | A micro-agent framework — single-responsibility agents built from a generic runtime plus a declarative skill file. Being migrated onto `spore-core` as its harness. |
| **Cordyceps** *(this project)* | A single autonomous, self-improving agent you deploy and control remotely. Built on `spore-core`, it uses `spore` skill files as one of its extension mechanisms. |
| **mycelium** *(future)* | Build and coordinate *teams* of agents — orchestration above the individual agent layer. |

```
spore-core  ──►  the harness runtime (reliability lives here)
   spore    ──►  micro agents: runtime + skill file
 cordyceps  ──►  one autonomous, self-improving operator   ◄── you are here
  mycelium  ──►  teams of agents working together
```

The biological metaphor runs through the family: spores germinate into something that grows, *Cordyceps* is the fungus that takes hold and acts, and a *mycelium* is the network that connects many of them.

---

## How It Works

Cordyceps is built on the **spore-core** harness, which means the hard parts of reliable agency — the agent loop, tool execution, sandbox isolation, context management, cross-session memory, verification sensors, and termination policy — are handled by the runtime. Cordyceps layers the *autonomous operator* concerns on top:

**Persistent daemon.** A long-running process that owns a task queue, durable state, and the agent's working memory. It survives restarts and resumes in-flight work.

**Remote control plane.** A channel to direct the agent from anywhere — submit tasks, adjust priorities, stream what it's doing, and approve sensitive actions. The interface is designed so the agent is useful whether you're watching closely or letting it run unattended.

**Self-extension.** When Cordyceps needs a capability it lacks, it can acquire one through two paths:
- **Skills** — declarative capability files (the `spore` skill-file format), added at runtime without touching core code.
- **Direct code changes** — modifying its own source, verifying the change, and bringing it into the running system.

Every self-improvement runs through the same verify-before-claim discipline the harness enforces for any task: the change is exercised and checked before it's trusted.

---

## Status

🌱 **Early planning.** This README describes the intended shape of the project. The architecture, control plane, and self-improvement mechanisms are being designed against `spore-core`. Interfaces and scope will firm up as the foundation lands.

---

## Safety & Autonomy

An always-on agent that can act on a machine and rewrite itself is powerful and demands guardrails. Design goals include:

- **Bounded autonomy** — explicit policy over what the agent may do unattended versus what requires approval.
- **Sandboxed execution** — capability boundaries enforced by the harness's sandbox layer, not by convention.
- **Auditable self-changes** — every self-modification is recorded, reviewable, and reversible.
- **Verify before trust** — no change to behavior or capability is accepted until it has been exercised and checked.

---

## License

[MIT](LICENSE) © SquirrelSoft LLC
