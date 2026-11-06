#!/bin/sh
set -eu

if [ "$#" -ne 6 ]; then
    echo "usage: $0 BINARY VERSION COMMIT CHANNEL PRIVATE_KEY OUTPUT" >&2
    exit 2
fi

binary=$1
version=$2
commit=$3
channel=$4
private_key=$5
output=$6
tag="v$version"

if [ "$channel" = stable ] && printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    :
elif [ "$channel" = beta ] && printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+-beta\.[0-9]+$'; then
    :
else
    echo "Version and channel must be MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-beta.N" >&2
    exit 2
fi

size=$(wc -c < "$binary" | tr -d ' ')
sha256=$(sha256sum "$binary" | awk '{print $1}')
published_at=$(date -u +'%Y-%m-%dT%H:%M:%SZ')
payload="$output.payload"
signature="$output.signature"

jq -cn \
    --arg channel "$channel" \
    --arg version "$version" \
    --arg tag "$tag" \
    --arg commit "$commit" \
    --arg sha256 "$sha256" \
    --arg published_at "$published_at" \
    --arg binary_url "https://github.com/jesther-ai/open-u60-pro/releases/download/$tag/zte-agent" \
    --arg notes_url "https://github.com/jesther-ai/open-u60-pro/releases/tag/$tag" \
    --argjson size "$size" \
    '{schema:1,channel:$channel,version:$version,tag:$tag,commit:$commit,target:"aarch64-unknown-linux-musl",size:$size,sha256:$sha256,binary_url:$binary_url,notes_url:$notes_url,published_at:$published_at,minimum_updater_version:"2.4.0-beta.1"}' \
    > "$payload"

openssl dgst -sha256 -sign "$private_key" -out "$signature" "$payload"
payload_b64=$(base64 < "$payload" | tr -d '\n')
signature_b64=$(base64 < "$signature" | tr -d '\n')
jq -cn --arg payload "$payload_b64" --arg signature "$signature_b64" \
    '{schema:1,key_id:"ota-v1",payload:$payload,signature:$signature}' > "$output"
rm -f "$payload" "$signature"
