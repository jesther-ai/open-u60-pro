# Agent OTA updates

The U60 Pro agent can update itself without a Mac companion after one successful
`setup.sh` or `deploy.sh` bootstrap. Those scripts install the A/B slot layout
and the small boot supervisor. OTA restarts the agent only; it does not reboot
the router or intentionally interrupt Wi-Fi or cellular data.

For the first Beta hardware test, bootstrap the signed prerelease artifact rather
than an unsigned local development build:

```sh
ZTE_AGENT_DOWNLOAD_URL=https://github.com/jesther-ai/open-u60-pro/releases/download/v2.4.0-beta.1/zte-agent ./setup.sh
```

## Safety model

- Release metadata is signed with an offline P-256 key embedded in release
  builds. A checksum alone is not accepted.
- The manifest may reference only immutable release assets in
  `jesther-ai/open-u60-pro`, and downloads have strict size limits.
- The binary is streamed to the inactive slot, checked for its exact size and
  SHA-256 digest, made executable, and probed before installation is offered.
- Installation is blocked during a speed test, recent eSIM switch, voice call,
  USSD session, critically low unplugged battery, or another update operation.
- The supervisor observes the candidate for 60 seconds. A crash, an unavailable
  local listener, or a power loss before commit returns to the previous slot.
- A failed version is quarantined so automatic checks do not repeatedly stage
  it. The last successful Stable build is also retained separately.
- Every modifying endpoint requires the configured agent password. Automatic
  checking is enabled by default; automatic download is opt-in; automatic
  installation is deliberately unsupported for now.

## Release lanes

- `vMAJOR.MINOR.PATCH` publishes a Stable release.
- `vMAJOR.MINOR.PATCH-beta.N` publishes a Beta prerelease without replacing
  GitHub's latest Stable release.
- `v2.4.0-beta.1` is the minimum updater version so the first manually
  bootstrapped Beta build can receive later Beta and Stable releases.
- Beta devices consider both lanes and choose the newer version. Returning to
  Stable discards any Beta candidate and can return from a Beta build to the
  current Stable build.

The release workflow writes signed `stable.json` and `beta.json` pointers to the
`ota-channels` branch. Devices never use GitHub's mutable `latest` redirect for
OTA selection.

## Signing-key setup

Generate the key once on a trusted offline machine:

```sh
openssl ecparam -name prime256v1 -genkey -noout -out ota-private.pem
```

Store the complete PEM as the repository Actions secret
`OTA_SIGNING_KEY_PEM`. Keep the private key out of Git, build artifacts and the
router. For a tagged release, CI derives and embeds only the public key, builds
the static ARM64 binary, signs the canonical manifest and updates the
appropriate lane. A release tag fails closed when the secret is absent, invalid
or not a P-256 key.

## Hardware validation before enabling broader rollout

On a physical U60 Pro, verify the first SSH bootstrap, boot persistence, A/B
symlink updates on `/data`, a healthy manual update, forced candidate failure,
power loss during the trial, rollback, low-battery deferral and continued WAN,
DNS and WebUI availability. Automatic installation should remain disabled until
this checklist passes on the target firmware.
