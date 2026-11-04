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
  A read that makes no progress for 30 seconds is stopped, the complete
  transfer has a five-minute budget, and partial files are removed immediately
  and again at startup.
- Install repeats the signed size, digest and compatibility checks. Damage to a
  staged binary or its metadata therefore cannot create a pending boot trial.
- Installation is blocked during a speed test, recent eSIM switch, voice call,
  USSD session, critically low unplugged battery, or another update operation.
- The supervisor observes the candidate for 60 seconds. A crash, an unavailable
  local listener, or a power loss before commit returns to the previous slot.
- At boot the supervisor verifies the stored size, SHA-256 digest and executable
  probe for every managed slot. It skips quarantined releases and can start the
  separately retained last Stable binary when neither A/B choice is safe.
- A fresh SSH/USB bootstrap seeds both slots and records a digest for each copy,
  so accidental corruption is detected before the first native OTA update.
- A failed version is quarantined so automatic checks do not repeatedly stage
  it. The last successful Stable build is also retained separately.
- Every modifying endpoint requires the configured agent password. Automatic
  checking is enabled by default; automatic download is opt-in; automatic
  installation is deliberately unsupported for now.

The download endpoint runs in the slow-work lane. A slow carrier or release
server does not consume the permits used by update status, the WebUI and normal
read-only API calls.

## Failure behavior

| Scenario | Result |
| --- | --- |
| Missing, malformed or incorrectly signed channel data | No binary is downloaded; the running agent is unchanged. |
| Slow or stalled download | A 30-second idle or five-minute total limit aborts it; partial files are deleted. |
| Truncated, oversized or wrong-hash payload | The inactive-slot download is rejected before staging. |
| Staged binary or metadata changes after download | Install preflight fails before writing a pending marker or stopping the agent. |
| Update-helper cannot start or confirm its handoff | The helper is stopped, the pending marker is cleared and the current agent keeps running. |
| Candidate cannot start, listen, or remain healthy | It is quarantined and the verified previous slot is restarted. |
| Reboot or power loss during the trial | The unproven candidate is excluded and the verified previous slot is selected. |
| Active slot is damaged at a later boot | The supervisor selects the other verified, non-quarantined slot. |
| Neither A/B slot is safe | The separately retained, verified last Stable copy is started without marking a broken A/B slot active. |

There is one deliberate recovery boundary: if both A/B binaries and the last
Stable rescue copy are all missing or corrupted, no on-device updater can run.
Recovery then requires SSH/USB and `setup.sh`. Validation must not deliberately
destroy all three copies on a production router.

## Release lanes

- `main` is the production branch. A `vMAJOR.MINOR.PATCH` Stable tag is
  accepted only when it points at the exact current `main` head.
- `beta` is the permanent pre-release branch. Feature changes should reach it
  first, and a `vMAJOR.MINOR.PATCH-beta.N` tag is accepted only when it points
  at the exact current `beta` head. Beta releases never replace GitHub's latest
  Stable release.
- Promote a tested release through a `beta` → `main` pull request. After the
  promotion merge and Stable release, bring `main` back into `beta` before the
  next development cycle so both lanes share the production fixes.
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
