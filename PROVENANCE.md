# Source Provenance

Tsubasa Redis Manager is an independent implementation. It is not affiliated
with, endorsed by, or sponsored by AnotherRedisDesktopManager (ARDM) or its
authors. ARDM is named in this repository only to describe compatibility and
to acknowledge inspiration (see [ACKNOWLEDGEMENTS.md](ACKNOWLEDGEMENTS.md)).

**No source code, styles, translations, assets, or tests from ARDM are in this
repository.**

## Where each part comes from

| Part | Origin |
|---|---|
| Frontend (`src/`, `index.html`, build config) | Written in this repository, starting on 2026-07-23 from a fresh Vue 3, TypeScript, and Vite scaffold. |
| Backend (`src-tauri/`) | A new Rust and Tauri implementation by the same author, first written in an earlier, unpublished prototype and moved here. ARDM is an Electron application with no Rust code to derive from. |
| Application icon | Original artwork. `src-tauri/icons/icon.svg` is the source; every other file in that directory is generated from it. |
| Documentation, HTML prototypes, tests | Written in this repository. |
| Third-party libraries | Used only as npm and Cargo dependencies; none is vendored. Listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md), with license texts in [THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt). |

The prototype also contained a copy of ARDM's Vue 2 frontend, modified to run
on the new backend through an ioredis-style wrapper that called Tauri
commands. That frontend and its wrapper were not copied, adapted, or migrated
into this repository: no Vue 2 components or JavaScript modules, no Webpack or
Babel configuration, no Element UI themes or fonts, no stylesheets, images, or
translations, no tests or documentation, and none of the prototype's Git
history. Only the files written for the new backend were moved, with project
names, crate names, bundle identifiers, and keyring service identifiers
renamed.

Because the backend first served that frontend, some of its original commands
followed the frontend's needs. That is an interface relationship, not shared
code, and the generic command passthrough the wrapper relied on has since
been removed: outside the CLI tab, this frontend calls only purpose-built
commands.

## Compatibility is not derivation

ARDM is a compatibility target, not a source. This project may match:

- user-visible functionality and Redis protocol behavior;
- ARDM's connection export format, so that existing connections can be
  imported;
- the placeholder convention of ARDM's custom formatters (`{VALUE}`, `{HEX}`,
  `{HEX_FILE}`, `{KEY}`), so that existing formatter commands keep working.

Component structure, styling, wording, assets, and tests are authored
independently and must stay that way.

## Rules for contributions

- Do not copy, port, or adapt ARDM's frontend code. New frontend code must be
  authored independently.
- Do not vendor third-party source. If an exception is ever unavoidable,
  record its license and copyright notice in the same commit
  (the `MANUAL_NOTES` section of `scripts/dev/gen-notices.mjs`, which
  regenerates `THIRD_PARTY_NOTICES.md`).
