# Extension Pack HTTP Ingress And `public_base_url`

This note is meant to be Codex-friendly and answer one concrete question:

How does an extension pack ask for HTTP ingress, and how does it get
`public_base_url` injected?

## Short answer

There is no separate `"ingress-http": true` switch today.

An extension pack effectively asks for HTTP ingress by combining:

1. A pack-declared public web/static surface when needed
   via the `greentic.static-routes.v1` extension.
2. A setup/config field named `public_base_url` when the pack needs an
   externally reachable base URL for callbacks, webhooks, or public UI links.
3. The appropriate ingress capability or host contract
   such as messaging ingress handling.

## 1. Requesting public HTTP surface

If the pack needs the host to expose public HTTP/static content, it declares
the static routes extension:

- extension key: `greentic.static-routes.v1`
- parser/validator:
  [static_routes.rs](/home/vgrishkyan/greentic/greentic-pack/crates/greentic-pack/src/static_routes.rs)

This is what the host/runtime inspects to decide whether the bundle has
bundle-level static routes and therefore needs public HTTP serving.

Relevant runtime inspection:

- [startup_contract.rs](/home/vgrishkyan/greentic/greentic-start/src/startup_contract.rs)

Important point:

- a pack requests HTTP/public web surface declaratively through the
  static-routes extension
- not through an ad-hoc runtime flag

## 2. Requesting `public_base_url`

If the pack needs a public URL injected into setup/runtime config, it should
declare a setup question or config field named `public_base_url`.

Examples already in the repo:

- [messaging-webchat/setup.yaml](/home/vgrishkyan/greentic/greentic-messaging-providers/packs/messaging-webchat/setup.yaml)
- [messaging-slack/setup.yaml](/home/vgrishkyan/greentic/greentic-messaging-providers/packs/messaging-slack/setup.yaml)
- [messaging-webex/setup.yaml](/home/vgrishkyan/greentic/greentic-messaging-providers/packs/messaging-webex/setup.yaml)

This means:

- the pack is saying "I need an externally reachable base URL"
- setup/onboarding can then ask for it or inject it from runtime/tunnel state

## 3. Where `public_base_url` comes from

There are two sources:

1. Explicit setup answers:
   - `platform_setup.static_routes.public_base_url`
   - provider-level setup answers containing `public_base_url`
2. Runtime-discovered public URL:
   - tunnel/public endpoint discovered by runtime and written back into runtime state

Relevant code:

- static routes normalization and validation:
  [platform_setup.rs](/home/vgrishkyan/greentic/greentic-setup/src/platform_setup.rs)
- setup persistence:
  [engine.rs](/home/vgrishkyan/greentic/greentic-setup/src/engine.rs)
- runtime startup contract:
  [startup_contract.rs](/home/vgrishkyan/greentic/greentic-start/src/startup_contract.rs)
- runtime public URL handling:
  [runtime.rs](/home/vgrishkyan/greentic/greentic-start/src/runtime.rs)

## 4. How it gets injected

For provider/setup execution, `public_base_url` is injected into the setup
payload/config when available.

Relevant code:

- [providers.rs](/home/vgrishkyan/greentic/greentic-start/src/providers.rs)

Current behavior there:

- if `public_base_url` is known, it is inserted into both:
  - top-level payload field `public_base_url`
  - nested `config.public_base_url`

This makes it easy for pack code to consume it regardless of whether the setup
component expects it at the top level or inside config.

## 5. How webhook-style ingress uses it

Webhook-oriented packs typically need `public_base_url` so the host can build a
callback URL.

Examples:

- [webhook.rs](/home/vgrishkyan/greentic/greentic-setup/src/webhook.rs)
- [webhook_setup.rs](/home/vgrishkyan/greentic/greentic-start/src/onboard/webhook_setup.rs)

The host builds URLs like:

- messaging webhook:
  `"{public_base_url}/v1/messaging/ingress/{provider_id}/{tenant}/{team}"`

So for webhook-driven packs, the practical contract is:

1. declare/setup `public_base_url`
2. expose the relevant ingress capability
3. let setup/runtime compose the final callback URL

## 6. Recommended pack authoring rule

If a pack needs public HTTP ingress or externally reachable callback URLs:

1. Declare `greentic.static-routes.v1` if it needs bundle-level public web/static surface.
2. Declare a `public_base_url` setup/config field if it needs an external base URL.
3. Optionally declare `ingress_path` if the pack wants a configurable path suffix.
4. Implement the relevant ingress capability (`messaging.provider_ingress.v1`, etc.).

## 7. One-line answer

Today an extension pack requests HTTP ingress declaratively through
`greentic.static-routes.v1` plus its ingress capability, and it receives
`public_base_url` by declaring that field in setup/config so setup/runtime can
inject the resolved public URL into the provider payload.
