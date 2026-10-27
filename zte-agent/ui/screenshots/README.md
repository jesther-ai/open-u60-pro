# Responsive console previews

These PNGs are screenshots of the real console using fictional local fixtures. All network addresses, device names, MACs and profile identifiers are demonstration values. No live router, SMS, credentials, activation codes or private configuration was used. eSIM ICCIDs are masked in the rendered UI.

Regenerate with Playwright and Chrome installed:

```sh
node zte-agent/ui/tests/screenshots.cjs
```

Playwright can also be made available through `NODE_PATH`. The fixture listens on loopback only and never contacts a router.
