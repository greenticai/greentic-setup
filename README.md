# greentic-setup

`greentic-setup` helps you take a Greentic bundle from "I have some packs" to
"this bundle is configured and ready to run".

This repository is for people who want a clear, practical setup tool:

- programmers who are new to Greentic
- product builders who are comfortable editing JSON but do not want to learn
  every internal runtime detail first
- team members who need to understand what setup changes are being made to a
  bundle

If you are a coding agent or are working with one, read
[Coding Agents Guide](docs/coding-agents.md) before changing code or debugging
cross-repo workflows.

## What This Project Does

`greentic-setup` is responsible for bundle setup.

In simple terms, that means it can:

- read a bundle and discover the packs inside it
- ask setup questions, either interactively or from an answers file
- persist provider configuration and secrets
- write tenant and team access rules
- prepare the local bundle state that the runtime expects

It is not the runtime itself, and it is not the bundle authoring tool.

## The Big Picture

If you are new to the Greentic toolchain, this is the easiest way to think
about it:

1. `greentic-pack` builds individual `.gtpack` files.
2. `greentic-bundle` or `gtc wizard apply` creates a bundle workspace and
   records which app packs and provider packs belong in it.
3. `greentic-setup` configures that bundle for a tenant, team, and environment.
4. `greentic-start` runs the configured bundle.
5. `gtc` is the main user-facing command that can call the other tools for you.

You usually use `gtc`.

You come to this repository when:

- setup is behaving strangely
- the wrong secrets or answers are being written
- tenant or team scoping looks wrong
- a bundle config step succeeds or fails unexpectedly

## Who Should Read What

Start with this `README.md` if you want:

- a plain-language explanation of the repo
- a short path to common commands
- a clear mental model of what setup owns

Read [docs/admin-api.md](docs/admin-api.md) if you want:

- the mTLS admin contract
- runtime-facing request and response shapes

Read [docs/extension-pack-ingress-http.md](docs/extension-pack-ingress-http.md)
if you want:

- details about `public_base_url`
- how HTTP ingress expectations flow from packs into runtime setup

Read [docs/coding-agents.md](docs/coding-agents.md) if you are:

- modifying code in this repo
- debugging workflows that span `gtc`, `greentic-bundle`, `greentic-pack`,
  `greentic-start`, `greentic-dev`, or the runner

## What A Bundle Looks Like

Modern Greentic bundle workspaces use `bundle.yaml` at the root.

You will commonly see directories such as:

- `bundle.yaml`: the authored bundle definition
- `bundle.lock.json`: normalized lock metadata
- `packs/`: app packs that live directly in the bundle
- `providers/`: extension or provider packs
- `tenants/`: tenant and team access rules
- `state/config/`: setup answers and persisted config artifacts
- `state/resolved/`: generated runtime-ready manifests
- `resolved/`: copied resolved manifests used by start flows

You do not need to memorize all of this to use the tool. It helps mostly when
you are debugging.

## What Setup Owns

`greentic-setup` owns things like:

- loading answers from JSON or YAML
- prompting for missing setup values
- honoring `--tenant`, `--team`, and `--env`
- writing setup answers and provider config
- persisting dev secrets in the local bundle state
- writing gmap tenant and team access rules
- preparing local resolved outputs that runtime startup expects

`greentic-setup` does not own every step in the full developer workflow.

For example:

- building `.gtpack` files belongs to `greentic-pack`
- materializing bundle composition from authored app-pack references belongs to
  `greentic-bundle`
- starting and running the bundle belongs to `greentic-start`
- top-level orchestration for users usually belongs to `gtc`

That separation matters when you are debugging. A setup symptom is not always a
setup bug.

## Common Human Workflows

### 1. Check the CLI

```bash
greentic-setup --help
greentic-setup bundle --help
```

### 2. Configure an Existing Bundle Interactively

```bash
greentic-setup ./my-bundle
```

This is the simplest mode.

It will:

- inspect the bundle
- ask setup questions
- show a plan
- apply the setup

### 3. Preview Setup Without Changing Anything

```bash
greentic-setup --dry-run ./my-bundle
```

Use this when you want to see what would happen before writing files.

### 4. Generate an Answers Template

```bash
greentic-setup --dry-run --emit-answers answers.json ./my-bundle
```

This is useful when:

- you want a repeatable setup process
- you want to review the expected fields with another teammate
- you want to check the exact provider questions without running setup yet

### 5. Apply a Saved Answers File

```bash
greentic-setup --answers answers.json ./my-bundle
```

This is the most common non-interactive path.

### 6. Run Advanced Bundle Commands

```bash
greentic-setup bundle init ./my-bundle --name "My Bundle"
greentic-setup bundle add ./some-pack.gtpack --bundle ./my-bundle
greentic-setup bundle status --bundle ./my-bundle
greentic-setup bundle setup --bundle ./my-bundle --answers answers.json
greentic-setup bundle update --bundle ./my-bundle --answers answers.json
greentic-setup bundle remove messaging-telegram --bundle ./my-bundle --force
```

These commands are more explicit and are useful for scripting or debugging.

## A Simple Answers File

This is a small example, not a complete one:

```json
{
  "tenant": "demo",
  "team": "default",
  "env": "dev",
  "platform_setup": {
    "static_routes": {
      "public_web_enabled": true,
      "public_base_url": "http://127.0.0.1:8080",
      "public_surface_policy": "enabled",
      "default_route_prefix_policy": "pack_declared",
      "tenant_path_policy": "pack_declared"
    }
  },
  "setup_answers": {
    "messaging-webchat-gui": {
      "public_base_url": "http://127.0.0.1:8080"
    }
  }
}
```

You do not need every field for every bundle.

The actual questions depend on the packs inside the bundle.

## A Friendly Mental Model For Tenants

If you are not used to tenant-aware systems, here is the simplest way to think
about it:

- a tenant is the main customer or logical workspace
- a team is a smaller group inside that tenant
- setup answers and secrets may be written with tenant and team scope

If you pass:

```bash
greentic-setup --tenant demo --team default ./my-bundle
```

then you should expect setup output and persisted state to reflect:

- `tenant=demo`
- `team=default`

If the CLI header says one tenant but the persisted secrets land under another,
that is a real bug worth investigating.

## When Something Looks Wrong

Here are a few simple checks that help a lot:

### Check the bundle metadata

```bash
sed -n '1,200p' bundle.yaml
jq . bundle.lock.json
```

### Check resolved setup state

```bash
find state/config -maxdepth 3 -type f | sort
find state/resolved -maxdepth 3 -type f | sort
```

### Check the dev secrets store

```bash
sed -n '1,80p' .greentic/dev/.dev.secrets.env
```

### Check tenant and team rules

```bash
find tenants -maxdepth 4 -type f | sort
```

## Troubleshooting Questions

### "Why did setup ask me for values I thought were already in the bundle?"

Usually because:

- the bundle contains packs with setup questions but no saved answers yet
- the answers file is missing some fields
- the existing state is for a different tenant, team, or environment

### "Why does a runtime problem sometimes turn out not to be a setup bug?"

Because setup is only one phase.

The full path often looks like this:

1. author or build a pack
2. assemble a bundle
3. configure the bundle
4. start the runtime

If the wrong app pack is missing from `packs/`, that may be a bundle
materialization problem, not a setup persistence problem.

### "Should I use `gtc` or `greentic-setup` directly?"

Use `gtc` for normal day-to-day work.

Use `greentic-setup` directly when:

- you are debugging setup behavior itself
- you want to isolate a bug away from higher-level orchestration
- you are writing or fixing tests in this repository

## For Coding Agents

If you are making code changes here, do not rely on this `README.md` alone.

Read [docs/coding-agents.md](docs/coding-agents.md) first. It explains:

- which tool owns which phase
- how the runner relates to `gtc`, `greentic-dev`, `greentic-pack`,
  `greentic-bundle`, and `greentic-start`
- which bugs belong in this repo and which belong elsewhere
- how to run useful local checks without confusing setup with runtime

## Current Documentation Set

The current maintained documents in this repo are:

- `README.md`
- `docs/coding-agents.md`
- `docs/admin-api.md`
- `docs/extension-pack-ingress-http.md`
- `docs/adaptive-cards.md`
- `docs/mtls-setup.md`

Older demo walkthroughs and stale checklist docs were removed so the repo has a
smaller, more trustworthy surface.
