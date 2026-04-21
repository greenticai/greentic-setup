# Coding Agents Guide

This document is for coding agents and technical debuggers working inside
`greentic-setup`.

If you are a human user looking for the basics, read the main
[README](../README.md) first.

## Purpose

Use this guide to answer one practical question:

When a Greentic workflow is broken, which tool actually owns the problem?

That matters because `greentic-setup` lives in the middle of a larger workflow.

## The Toolchain At A Glance

The safest mental model is:

1. `greentic-pack` builds a pack into a `.gtpack`.
2. `greentic-bundle` assembles authored bundle metadata and materializes bundle
   dependencies such as app packs and extension providers.
3. `gtc` is the human-facing orchestrator and often shells out to the lower
   level tools.
4. `greentic-setup` configures the bundle for tenant, team, environment,
   provider answers, and secrets.
5. `greentic-start` runs the configured bundle.
6. the runner executes or hosts the running workflow and is used together with
   the runtime-oriented tools rather than as a replacement for them.
7. `greentic-dev` is the local developer toolbox for validating flows, local
   development loops, packaging helpers, and related development tasks.

## Ownership Boundaries

### `greentic-setup` owns

- loading answers files
- interactive setup prompts
- tenant, team, and environment scope handling
- persistence of provider config under `state/config`
- persistence of dev secrets under `.greentic/dev/.dev.secrets.env`
- gmap writes under `tenants/...`
- resolved local setup outputs that support startup
- setup-specific CLI behavior in `greentic-setup ...`

### `greentic-bundle` owns

- `bundle.yaml`
- `bundle.lock.json`
- app-pack and provider reference recording
- materializing app packs into `packs/` or tenant/team pack directories
- materializing provider packs into `providers/...`
- composition and bundle authoring semantics

### `greentic-pack` owns

- building or rebuilding `.gtpack` artifacts
- validating pack contents at pack-build time
- pack authoring concerns such as extensions and pack-level metadata

### `greentic-start` owns

- runtime lifecycle
- startup contract handling
- runtime ports, ingress, and serving behavior
- loading the prepared bundle into a running system

### `gtc` owns

- end-user orchestration
- command delegation between tools
- higher-level UX across setup, bundle creation, and runtime startup

### the runner owns

- executing the operational runtime path once the bundle has been authored and
  configured
- cooperating with `greentic-start` and related runtime tooling instead of
  replacing bundle creation or setup

## How To Use The Runner Correctly

Treat the runner as part of the runtime stage, not the setup stage.

That means:

- do not use the runner to prove that bundle authoring succeeded
- do not use the runner to prove that setup persistence succeeded unless the
  authored bundle and runtime startup path are already known-good
- when debugging runner behavior, first verify the bundle metadata and setup
  outputs locally

In practice, the usual order is:

1. build or obtain packs
2. create or update the bundle
3. run setup
4. start the runtime
5. observe runner behavior

If step 2 failed, the runner is downstream and will only show you symptoms.

## How The Tools Fit Together

### Human-oriented path

Typical user path:

1. `gtc wizard apply` or `greentic-bundle wizard apply`
2. `gtc setup` or direct `greentic-setup`
3. `gtc start` or the runtime-specific start flow

### Debugging path

When isolating ownership, prefer:

1. direct `greentic-bundle` to test bundle creation and materialization
2. direct `greentic-setup` to test setup behavior
3. direct `greentic-start` to test runtime behavior

That prevents a bug in `gtc` orchestration from hiding the real owner.

## Fast Ownership Heuristics

Use these rules before making code changes.

### If `bundle.yaml` or `bundle.lock.json` is wrong

Suspect:

- `greentic-bundle`
- `gtc` answer replay or delegation logic

Do not start in `greentic-setup`.

### If an app pack reference exists in `bundle.yaml` but the app pack is not
materialized into `packs/`

Suspect:

- `greentic-bundle`

That is bundle composition and materialization, not setup persistence.

### If setup prints the wrong tenant or persists secrets under the wrong tenant

Suspect:

- `greentic-setup`

This repo owns tenant-aware setup scope resolution and persistence.

### If the bundle is configured correctly but the wrong port, route, or ingress
is actually served

Suspect:

- `greentic-start`
- runner or runtime integration

### If the wrong lower-level tool is being invoked at all

Suspect:

- `gtc`

## Recommended Debugging Sequence

When you receive a bug report, follow this order:

1. inspect `bundle.yaml`
2. inspect `bundle.lock.json`
3. inspect `packs/` and `providers/`
4. run direct `greentic-setup`
5. inspect `state/config`, `state/resolved`, `.greentic/dev/.dev.secrets.env`
6. only then test `greentic-start` or runner behavior

This sequence avoids blaming setup for bundle-authoring problems or blaming the
runtime for setup-persistence problems.

## Useful Local Commands

### Build and test this repo

```bash
cargo build
cargo test
cargo clippy -- -D warnings
bash ci/local_check.sh
```

### Isolate a setup bug

```bash
greentic-setup --answers answers.json --tenant demo ./my-bundle
greentic-setup bundle setup --bundle ./my-bundle --answers answers.json --non-interactive
```

### Inspect bundle state

```bash
sed -n '1,220p' bundle.yaml
jq . bundle.lock.json
find packs providers state/config state/resolved tenants -maxdepth 4 -type f | sort
```

### Rebuild a pack before blaming setup

```bash
greentic-pack build --in . --gtpack-out ./dist/my-pack.gtpack
```

### Check runtime only after setup is known-good

Use the appropriate `gtc start`, `greentic-start`, or local runtime command for
the repo you are debugging. The exact startup command is owned by the runtime
layer, not by `greentic-setup`.

## Current Repo-Specific Notes

### Do not assume `gtc` is just a transparent passthrough

It may add orchestration behavior, answer transformation, or call a different
tool than you expect.

### Do not assume a setup symptom is a setup root cause

A missing app pack, for example, may come from bundle materialization and only
become visible during setup.

### Do not use stale pre-`bundle.yaml` documentation

This repo now treats `bundle.yaml` and `bundle.lock.json` as the current bundle
workspace markers.

## When To File A Bug In Another Repo

Open or hand off a bug outside this repo when:

- app-pack references are recorded correctly but not materialized into `packs/`
- runtime startup ignores already-correct setup outputs
- `gtc` forwards the wrong arguments or reinterprets answers incorrectly
- a `.gtpack` is missing required pack metadata and needs rebuilding

## If You Need To Explain This To Another Agent

Use this short version:

`greentic-setup` configures bundles. It does not author packs, does not own
bundle composition, and does not own runtime startup. Verify bundle metadata and
materialized packs first, then test setup persistence, then test runtime.
