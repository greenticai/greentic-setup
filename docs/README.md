# Documentation Index

This directory contains the maintained documents for `greentic-setup`.

## Start Here

- [README](../README.md)
  Human-friendly overview of the repository, what setup owns, and the most
  common workflows.

## For Coding Agents

- [coding-agents.md](./coding-agents.md)
  Ownership boundaries across `gtc`, `greentic-bundle`, `greentic-pack`,
  `greentic-start`, `greentic-dev`, the runner, and `greentic-setup`.

## Reference Guides

- [admin-api.md](./admin-api.md)
  Shared mTLS admin API contract and runtime-facing request and response types.

- [adaptive-cards.md](./adaptive-cards.md)
  Notes about adaptive-card-based setup flows.

- [extension-pack-ingress-http.md](./extension-pack-ingress-http.md)
  Human-readable explanation of HTTP ingress and `public_base_url`.

- [mtls-setup.md](./mtls-setup.md)
  mTLS certificate setup guidance.

## Documentation Policy

This repo intentionally keeps a smaller documentation surface than before.

If a document:

- describes an outdated bundle layout
- assumes old passthrough behavior
- duplicates newer docs without adding clear value

it should usually be removed or merged rather than left behind.
