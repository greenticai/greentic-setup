# Extension Pack HTTP Ingress And `public_base_url`

This guide answers a simple question:

How does a pack say "I need a public HTTP surface" and how does it receive
`public_base_url`?

This document is written for humans first. You do not need to know every
runtime detail to follow it.

## Short Answer

There is no single `ingress-http = true` switch.

A pack effectively asks for HTTP ingress by combining:

1. a declared public web or static surface
2. a setup field called `public_base_url` when it needs an externally reachable
   URL
3. the correct ingress-related capability or host behavior for that pack type

In practice, a pack says:

- "please expose me publicly"
- "please tell me what public URL users or callbacks should use"

## What `public_base_url` Means

`public_base_url` is the base URL that outside systems can reach.

Examples:

- `http://127.0.0.1:8080` for a simple local setup
- `https://my-demo.example.com` for a deployed environment
- a temporary tunnel URL when you are testing locally

Packs often need this when they:

- serve a public web UI
- need webhook callbacks
- generate links that users open in a browser

## How A Pack Requests Public HTTP Surface

If a pack needs the host to expose public HTTP or static content, it declares
that through its pack metadata, usually via the static-routes extension.

Important idea:

- public serving is declared by the pack
- it is not meant to be an ad-hoc runtime-only toggle

That allows the host and setup flow to reason about whether a bundle needs:

- public static content
- public web routes
- a public base URL

## How A Pack Requests `public_base_url`

If a pack needs a public URL, it should expose a setup or config field named
`public_base_url`.

That tells the setup and runtime layers:

- this pack expects a public base URL
- that URL may come from explicit setup answers or from runtime-discovered
  tunnel state

This is the normal contract for packs that need browser-facing links or
webhook callbacks.

## Where `public_base_url` Comes From

There are two common sources.

### 1. Explicit setup answers

Examples:

- `platform_setup.static_routes.public_base_url`
- provider-specific setup answers that include `public_base_url`

This is the easiest model to understand:

- the user or deployer already knows the public URL
- they supply it during setup

### 2. Runtime-discovered public URL

Sometimes the public URL is discovered at startup time.

For example:

- a tunnel is created
- the runtime learns the public endpoint
- runtime state is updated with that public URL

This is common in local development when a public callback URL is needed but no
fixed domain exists yet.

## How Injection Works

When `public_base_url` is known, it can be passed into provider setup or runtime
config payloads.

The exact shape depends on the consumer, but the important human-level idea is:

- packs do not usually need to discover the public URL by themselves
- the host or runtime can inject it once it is known

This makes pack behavior more predictable and avoids each pack inventing a
different way to ask for the same information.

## Why Webhook Packs Need It

Webhook-driven integrations need a callback URL that outside services can call.

That callback URL is usually built from:

- `public_base_url`
- a route pattern
- tenant and team information when applicable

Conceptually, it looks like:

```text
{public_base_url}/v1/.../ingress/.../{tenant}/{team}
```

So when a pack needs inbound callbacks, the real dependency is not just
"HTTP ingress". It is:

1. a public route
2. a public base URL
3. the correct ingress behavior

## Recommended Rule For Pack Authors

If your pack needs public ingress or callback URLs, follow this pattern:

1. Declare the public web or static surface in pack metadata.
2. Add a `public_base_url` setup field if the pack needs an externally
   reachable URL.
3. Add any extra route field only when you genuinely need configurable path
   behavior.
4. Implement the correct ingress capability for the pack type.

This gives setup and runtime one consistent way to help the pack.

## Common Misunderstanding

It is easy to think:

"If the pack needs inbound HTTP, there must be one special ingress flag."

But the real contract is broader than that.

A pack usually needs:

- public exposure
- URL knowledge
- routing behavior

That is why `public_base_url` matters so much.

## Practical Summary

If a pack needs a public UI or callback URL:

- declare the public surface
- declare `public_base_url`
- let setup or runtime inject the resolved URL

That is the current Greentic model.
