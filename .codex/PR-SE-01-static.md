PR-SE-01 — Setup support for static hosting policy
Title

Add bundle-level static hosting policy collection and persistence to greentic-setup

Goal

Make setup collect and persist the stable hosting policy required for static routes, without defining per-pack route tables.

Why

Setup should own:

policy

normalization

replayable answers

emitted config artifact

Setup should not own:

runtime route assembly

pack route declaration

live route serving

Scope
Add setup questions

Add bundle/environment-level questions for static hosting policy, only when needed or enabled by profile:

public_web_enabled

public_base_url

public_surface_policy

optional default_route_prefix_policy

optional tenant_path_policy

Validation

Add setup-time validation for:

valid normalized URL format for public_base_url

required combinations, e.g. public_web_enabled=true requires public_base_url

environment/profile policy compatibility

normalized prefix/path policy consistency

Persist a bundle-level artifact

Create a dedicated artifact, for example:

state/config/platform/static-routes.json

Suggested shape:

{
  "version": 1,
  "public_web_enabled": true,
  "public_base_url": "https://example.com",
  "public_surface_policy": "enabled",
  "default_route_prefix_policy": "pack_declared",
  "tenant_path_policy": "pack_declared"
}
Replay support

Ensure --emit-answers includes the same static hosting fields so setup/update is replayable.

Update flow

Support:

initial disabled state

later enablement through update flow

changing base URL/policy later without hidden runtime-only toggles

Non-goals

no route collision detection

no static route schema parsing

no serving logic

no operator-side mount logic

Files likely touched

setup question generation

setup input validation

emitted answers logic

setup execution persistence

docs/examples for setup/update

Acceptance criteria

setup can collect static hosting policy

policy is persisted bundle-level

replay/update flows include it

no per-pack route metadata is duplicated into setup outputs