# Third-party notices

OpenPiso's license does not replace the licenses of its dependencies, derived
components, icons, or fonts. Preserve their copyright and license notices when
redistributing. Versions below are resolved in the committed lockfiles.

## Mozilla Public License 2.0

Windows binaries include the following **unmodified** Rust crates. Their source
is available from the versioned crates.io links below. Covered source remains
under [MPL-2.0](https://www.mozilla.org/MPL/2.0/); recipients may obtain that source
and exercise the rights granted by the MPL. MPL does not license OpenPiso's
separate source files. Upstream copyright and notices are in each crate archive.

| Crate | Source |
| --- | --- |
| serialport 4.9.0 | https://crates.io/crates/serialport/4.9.0 |
| cssparser 0.36.0 | https://crates.io/crates/cssparser/0.36.0 |
| selectors 0.36.1 | https://crates.io/crates/selectors/0.36.1 |
| cssparser-macros 0.6.1 | https://crates.io/crates/cssparser-macros/0.6.1 |
| dtoa-short 0.3.5 | https://crates.io/crates/dtoa-short/0.3.5 |
| option-ext 0.2.0 | https://crates.io/crates/option-ext/0.2.0 |

`unescaper` 0.1.8 is dual GPL-3.0/MIT and used **under MIT**. It is a Linux-only
serialport dependency, not part of Windows builds. No GPL option is selected
for Windows distribution.

## Frontend, components, and fonts

| Component | License and upstream notices/source |
| --- | --- |
| DOMPurify 3.4.17 | Apache-2.0 option (not its alternative MPL option); copyright Cure53 and contributors. [License](https://github.com/cure53/DOMPurify/blob/main/LICENSE), [source](https://github.com/cure53/DOMPurify) |
| Inter (`@fontsource-variable/inter` 5.2.8) | OFL-1.1; copyright 2016 The Inter Project Authors. [License/source](https://github.com/rsms/inter/blob/master/LICENSE.txt); installed package `LICENSE` |
| Lucide icons (`lucide-svelte`) | ISC; copyright Lucide Contributors, with Feather-derived icons copyright Cole Bemis. [Notices/license](https://github.com/lucide-icons/lucide/blob/main/LICENSE) |
| shadcn-svelte-derived UI components | MIT; copyright 2023 Hunter Johnston, CokaKoala, and shadcn. [License/source](https://github.com/huntabyte/shadcn-svelte/blob/main/LICENSE.md) |
| Noto Sans wordmark | OFL-1.1; copyright 2022 The Noto Project Authors. Full existing notices: `apps/client/public/OFL.txt`, `apps/web-admin/src/lib/assets/logo/OFL.txt` |
| svelte-toolbelt 0.10.6 | MIT; copyright 2024 Hunter Johnston and Thomas G. Lopes. [License/source](https://github.com/huntabyte/svelte-toolbelt/blob/main/LICENSE) |

Remaining Rust and JavaScript dependencies retain their own permissive licenses
(MIT, Apache-2.0, ISC, BSD, Unicode, or Zlib). Inspect exact resolved versions,
license identifiers, and source locations with `cargo metadata --format-version 1`;
JavaScript package licenses and notices are in `node_modules` after `bun install`
(including Bun's `.bun` package store). Consult each package's license/NOTICE
files for complete terms and copyright notices before redistribution.
