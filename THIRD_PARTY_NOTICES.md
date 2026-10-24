# Third-party notices

zte-agent is licensed under the [MIT License](LICENSE). The binary built from this repository also contains the components below. This file accompanies every release of the `zte-agent` binary.

## euicc (LGPL-2.1-only)

The `euicc/` directory is a Rust translation of libeuicc, the eSIM library of [lpac](https://github.com/estkme-group/lpac) (commit `c7c8d4f`). It is linked into `zte-agent`.

```
Copyright (C) 2023-2025 ESTKME TECHNOLOGY LIMITED, Hong Kong
Copyright (C) 2026 Jesther Silvestre (Rust translation)
```

It covers the card session, profile management, notifications, card information and profile download (`es10b`/`es10c`, `download`, `es8p` metadata, `es9p` server protocol).

The euicc library and its use are covered by the **GNU Lesser General Public License, version 2.1 only**. A copy of the license is in [`euicc/LICENSE`](euicc/LICENSE) and is attached to each release as `LICENSE-LGPL-2.1.txt`. Each file in `euicc/src/` states which libeuicc file it was translated from and how it was changed.

**Source code and relinking.** The complete source code of the euicc library and of zte-agent for a release is in the repository at that release's tag (also the "Source code" archives on the release page): https://github.com/jesther-ai/open-u60-pro. To use a modified euicc library, edit `euicc/` and rebuild:

```sh
cargo build --release --target aarch64-unknown-linux-musl -p zte-agent
```

You may modify zte-agent for your own use and reverse engineer it to debug such modifications.

### cJSON (MIT), inside euicc

`euicc/src/es9p.rs` reads and writes JSON the way libeuicc's cJSON did; that part follows cJSON 1.7.18 (as pinned by lpac) and carries its notice:

```
Copyright (c) 2009-2017 Dave Gamble and cJSON contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

## QMI-over-QRTR transport (MIT)

`zte-agent/src/qrtr_uim.rs` is based on lpac's QMI-over-QRTR APDU driver (`driver/apdu/qmi_qrtr.c`, `qmi_common.c`).

```
Copyright (c) 2024, Luca Weiss <luca.weiss@fairphone.com>
Copyright (c) 2026, Jesther Silvestre

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## GSMA RSP2 Root CI1 certificate

`zte-agent/certs/gsma-rsp2-root-ci1.pem` is the public root certificate of the GSMA eSIM certificate issuer. It is embedded in zte-agent so that operator servers' TLS certificates can be verified. See [`zte-agent/certs/README.md`](zte-agent/certs/README.md) for how it was checked.

## Datastar (MIT)

`zte-agent/src/web/assets/datastar-1.0.4.js` is [Datastar](https://github.com/starfederation/datastar) v1.0.4, the release bundle `bundles/datastar.js` at tag `v1.0.4`, vendored byte-identical (SHA-256 `727844adfc825ee651fb93c544a2a739986f9a21820a94524b35f0cac470cf91`, checked by `zte-agent/build.rs`). It is embedded in zte-agent and served to browsers by the web UI. Its license, from `LICENSE.md` at that tag:

```
Copyright © Star Federation

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
```

## QR Code generator library (MIT)

The web UI's Share Wi-Fi page draws its join QR code with the [`qrcodegen`](https://crates.io/crates/qrcodegen) crate 1.8.0 ([QR Code generator library](https://www.nayuki.io/page/qr-code-generator-library), Rust version). The crate ships no separate license file; its notice, from the head of `src/lib.rs`:

```
QR Code generator library (Rust)

Copyright (c) Project Nayuki. (MIT License)
https://www.nayuki.io/page/qr-code-generator-library

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software is furnished to do so,
subject to the following conditions:
- The above copyright notice and this permission notice shall be included in
  all copies or substantial portions of the Software.
- The Software is provided "as is", without warranty of any kind, express or
  implied, including but not limited to the warranties of merchantability,
  fitness for a particular purpose and noninfringement. In no event shall the
  authors or copyright holders be liable for any claim, damages or other
  liability, whether in an action of contract, tort or otherwise, arising from,
  out of or in connection with the Software or the use or other dealings in the
  Software.
```

## quirc (ISC), inside rqrr

The web UI reads the eSIM QR code in an uploaded photo with the [`rqrr`](https://crates.io/crates/rqrr) crate 0.11.0 ((MIT OR Apache-2.0) AND ISC), which is based on quirc. Its `LICENSE-ISC`:

```
quirc -- QR-code recognition library
Copyright (C) 2010-2012 Daniel Beer <dlbeer@gmail.com>

ISC License
===========

Permission to use, copy, modify, and/or distribute this software for
any purpose with or without fee is hereby granted, provided that the
above copyright notice and this permission notice appear in all
copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL
WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL
DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR
PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER
TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR
PERFORMANCE OF THIS SOFTWARE.
```

## BSD-3-Clause crates (matchit, subtle)

Two of the Rust crates below are under the BSD 3-Clause License, which asks binary redistributions to reproduce the notice: `matchit` 0.8.4 (MIT AND BSD-3-Clause; its route tree derives from httprouter, `LICENSE.httprouter` in the crate) and `subtle` 2.6.1 (`LICENSE` in the crate).

matchit:

```
BSD 3-Clause License

Copyright (c) 2013, Julien Schmidt
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

subtle:

```
Copyright (c) 2016-2017 Isis Agora Lovecruft, Henry de Valence. All rights reserved.
Copyright (c) 2016-2024 Isis Agora Lovecruft. All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

1. Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright
notice, this list of conditions and the following disclaimer in the
documentation and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
contributors may be used to endorse or promote products derived from
this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS
IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED
TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A
PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED
TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## Rust crates

zte-agent links these crates from crates.io (for the router target), each under the license shown. Each crate's license text and copyright notices are in its published source.

| Crate | Version | License |
|---|---|---|
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| atomic-waker | 1.1.2 | Apache-2.0 OR MIT |
| axum | 0.8.9 | MIT |
| axum-core | 0.5.6 | MIT |
| base16ct | 0.2.0 | Apache-2.0 OR MIT |
| base64 | 0.22.1 | MIT OR Apache-2.0 |
| base64ct | 1.8.3 | Apache-2.0 OR MIT |
| bitflags | 2.13.2 | MIT OR Apache-2.0 |
| block-buffer | 0.10.4 | MIT OR Apache-2.0 |
| bytes | 1.11.1 | MIT |
| cfg-if | 1.0.4 | MIT OR Apache-2.0 |
| const-oid | 0.9.6 | Apache-2.0 OR MIT |
| cpufeatures | 0.2.17 | MIT OR Apache-2.0 |
| crc32fast | 1.5.0 | MIT OR Apache-2.0 |
| crypto-bigint | 0.5.5 | Apache-2.0 OR MIT |
| crypto-common | 0.1.7 | MIT OR Apache-2.0 |
| der | 0.7.10 | Apache-2.0 OR MIT |
| digest | 0.10.7 | MIT OR Apache-2.0 |
| displaydoc | 0.2.5 | MIT OR Apache-2.0 |
| ecdsa | 0.16.9 | Apache-2.0 OR MIT |
| elliptic-curve | 0.13.8 | Apache-2.0 OR MIT |
| equivalent | 1.0.2 | Apache-2.0 OR MIT |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 |
| ff | 0.13.1 | MIT/Apache-2.0 |
| flate2 | 1.1.9 | MIT OR Apache-2.0 |
| fnv | 1.0.7 | Apache-2.0 / MIT |
| foldhash | 0.2.0 | Zlib |
| form_urlencoded | 1.2.2 | MIT OR Apache-2.0 |
| futures-channel | 0.3.34 | MIT OR Apache-2.0 |
| futures-core | 0.3.34 | MIT OR Apache-2.0 |
| futures-io | 0.3.34 | MIT OR Apache-2.0 |
| futures-sink | 0.3.34 | MIT OR Apache-2.0 |
| futures-task | 0.3.34 | MIT OR Apache-2.0 |
| futures-util | 0.3.34 | MIT OR Apache-2.0 |
| g2gen | 1.2.2 | MIT/Apache-2.0 |
| g2p | 1.2.2 | MIT/Apache-2.0 |
| g2poly | 1.2.2 | MIT/Apache-2.0 |
| generic-array | 0.14.7 | MIT |
| getrandom | 0.2.17 | MIT OR Apache-2.0 |
| group | 0.13.0 | MIT/Apache-2.0 |
| h2 | 0.4.19 | MIT |
| hashbrown | 0.16.1 | MIT OR Apache-2.0 |
| hashbrown | 0.17.1 | MIT OR Apache-2.0 |
| hmac | 0.12.1 | MIT OR Apache-2.0 |
| http | 1.4.0 | MIT OR Apache-2.0 |
| http-body | 1.1.0 | MIT |
| http-body-util | 0.1.5 | MIT |
| httparse | 1.10.1 | MIT OR Apache-2.0 |
| httpdate | 1.0.3 | MIT OR Apache-2.0 |
| hyper | 1.11.1 | MIT |
| hyper-rustls | 0.27.9 | Apache-2.0 OR ISC OR MIT |
| hyper-util | 0.1.20 | MIT |
| icu_collections | 2.1.1 | Unicode-3.0 |
| icu_locale_core | 2.1.1 | Unicode-3.0 |
| icu_normalizer | 2.1.1 | Unicode-3.0 |
| icu_normalizer_data | 2.1.1 | Unicode-3.0 |
| icu_properties | 2.1.2 | Unicode-3.0 |
| icu_properties_data | 2.1.2 | Unicode-3.0 |
| icu_provider | 2.1.1 | Unicode-3.0 |
| idna | 1.1.0 | MIT OR Apache-2.0 |
| idna_adapter | 1.2.1 | Apache-2.0 OR MIT |
| indexmap | 2.13.0 | Apache-2.0 OR MIT |
| ipnet | 2.12.2 | MIT OR Apache-2.0 |
| itoa | 1.0.17 | MIT OR Apache-2.0 |
| jpeg-decoder | 0.3.2 | MIT OR Apache-2.0 |
| libc | 0.2.182 | MIT OR Apache-2.0 |
| litemap | 0.8.1 | Unicode-3.0 |
| log | 0.4.29 | MIT OR Apache-2.0 |
| lru | 0.18.5 | MIT |
| matchit | 0.8.4 | MIT AND BSD-3-Clause |
| maud | 0.27.0 | MIT OR Apache-2.0 |
| maud_macros | 0.27.0 | MIT OR Apache-2.0 |
| memchr | 2.8.0 | Unlicense OR MIT |
| mime | 0.3.17 | MIT OR Apache-2.0 |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 |
| mio | 1.1.1 | MIT |
| once_cell | 1.21.3 | MIT OR Apache-2.0 |
| p256 | 0.13.2 | Apache-2.0 OR MIT |
| pem-rfc7468 | 0.7.0 | Apache-2.0 OR MIT |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT |
| pkcs8 | 0.10.2 | Apache-2.0 OR MIT |
| png | 0.18.1 | MIT OR Apache-2.0 |
| potential_utf | 0.1.4 | Unicode-3.0 |
| primeorder | 0.13.6 | Apache-2.0 OR MIT |
| proc-macro2 | 1.0.106 | MIT OR Apache-2.0 |
| proc-macro2-diagnostics | 0.10.1 | MIT/Apache-2.0 |
| qrcodegen | 1.8.0 | MIT |
| quote | 1.0.44 | MIT OR Apache-2.0 |
| rand_core | 0.6.4 | MIT OR Apache-2.0 |
| reqwest | 0.12.28 | MIT OR Apache-2.0 |
| rfc6979 | 0.4.0 | Apache-2.0 OR MIT |
| ring | 0.17.14 | Apache-2.0 AND ISC |
| rqrr | 0.11.0 | (MIT OR Apache-2.0) AND ISC |
| rustls | 0.23.37 | Apache-2.0 OR ISC OR MIT |
| rustls-pki-types | 1.14.0 | MIT OR Apache-2.0 |
| rustls-webpki | 0.103.9 | ISC |
| ryu | 1.0.23 | Apache-2.0 OR BSL-1.0 |
| sec1 | 0.7.3 | Apache-2.0 OR MIT |
| serde | 1.0.228 | MIT OR Apache-2.0 |
| serde_core | 1.0.228 | MIT OR Apache-2.0 |
| serde_derive | 1.0.228 | MIT OR Apache-2.0 |
| serde_json | 1.0.149 | MIT OR Apache-2.0 |
| serde_urlencoded | 0.7.1 | MIT/Apache-2.0 |
| sha2 | 0.10.9 | MIT OR Apache-2.0 |
| signature | 2.2.0 | Apache-2.0 OR MIT |
| simd-adler32 | 0.3.8 | MIT |
| slab | 0.4.12 | MIT |
| smallvec | 1.15.1 | MIT OR Apache-2.0 |
| socket2 | 0.6.5 | MIT OR Apache-2.0 |
| spki | 0.7.3 | Apache-2.0 OR MIT |
| stable_deref_trait | 1.2.1 | MIT OR Apache-2.0 |
| subtle | 2.6.1 | BSD-3-Clause |
| syn | 2.0.117 | MIT OR Apache-2.0 |
| sync_wrapper | 1.0.2 | Apache-2.0 |
| synstructure | 0.13.2 | MIT |
| tinystr | 0.8.2 | Unicode-3.0 |
| tokio | 1.50.0 | MIT |
| tokio-macros | 2.6.1 | MIT |
| tokio-rustls | 0.26.5 | MIT OR Apache-2.0 |
| tokio-util | 0.7.19 | MIT |
| tower | 0.5.3 | MIT |
| tower-http | 0.6.11 | MIT |
| tower-layer | 0.3.3 | MIT |
| tower-service | 0.3.3 | MIT |
| tracing | 0.1.44 | MIT |
| tracing-core | 0.1.36 | MIT |
| try-lock | 0.2.5 | MIT |
| typenum | 1.19.0 | MIT OR Apache-2.0 |
| unicode-ident | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 |
| untrusted | 0.9.0 | ISC |
| ureq | 3.2.0 | MIT OR Apache-2.0 |
| ureq-proto | 0.5.3 | MIT OR Apache-2.0 |
| url | 2.5.8 | MIT OR Apache-2.0 |
| utf-8 | 0.7.6 | MIT OR Apache-2.0 |
| utf8_iter | 1.0.4 | Apache-2.0 OR MIT |
| want | 0.3.1 | MIT |
| webpki-roots | 1.0.6 | CDLA-Permissive-2.0 |
| writeable | 0.6.2 | Unicode-3.0 |
| yoke | 0.8.1 | Unicode-3.0 |
| yoke-derive | 0.8.1 | Unicode-3.0 |
| zerofrom | 0.1.6 | Unicode-3.0 |
| zerofrom-derive | 0.1.6 | Unicode-3.0 |
| zeroize | 1.8.2 | Apache-2.0 OR MIT |
| zerotrie | 0.2.3 | Unicode-3.0 |
| zerovec | 0.11.5 | Unicode-3.0 |
| zerovec-derive | 0.11.2 | Unicode-3.0 |
| zmij | 1.0.21 | MIT |
