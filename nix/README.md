# Nix modules

This directory holds the Nix expressions that back `flake.nix`. Most files
are self-explanatory (one per concern: `apps.nix`, `cache.nix`, etc.). This
README documents one recurring chore that's easy to get wrong the first time:
capturing the sha256 for an Ollama model so it can be pre-baked into a
content-addressed container image.

## Capturing a model sha256 for `containers.nix`

`nix/containers.nix` defines a `modelRegistry` with entries like:

```nix
"gemma" = {
  name = "gemma4:e4b";
  hash = "sha256-0000000000000000000000000000000000000000000="; # Placeholder
  ...
};
```

When `hash` is the all-zeros placeholder, `createModelDerivation` falls back
to a dev-mode stub (`$out/models/README` with a note, no real model). That's
fine for evaluation workflows that `ollama pull` at runtime, but it means
`nix build .#<model>-container` produces an image without the model baked
in, and nothing is content-addressed into the Cachix cache.

To replace a placeholder with a real, reproducible hash you need:

1. A machine with `nix` (>=2.4, flakes enabled) on PATH.
2. `ollama` installed locally - the fixed-output derivation shells out to it.
3. Outbound network access to `registry.ollama.ai`.

Then run the helper:

```sh
scripts/update-model-sha256.sh gemma
```

The script:

1. Verifies the model key exists and is currently holding the placeholder
   (use `FORCE=1` to overwrite an existing non-placeholder hash).
2. Temporarily swaps the placeholder for `lib.fakeSha256` (43 A's) so
   `createModelDerivation` routes to the fixed-output path instead of the
   dev-stub branch. The build is expected to fail with a hash mismatch
   and Nix reports the real hash as `got: sha256-...`.
3. Restores the original file, applies the captured real hash, and runs
   a confirmation build. On any failure (capture or validation) the trap
   restores `nix/containers.nix` from the backup.
4. Prints the diff so you can review before committing.

Verify with a clean build:

```sh
nix build .#gemma-model              # should succeed now
nix build .#gemma-model-strict       # also succeeds; throws if hash is still placeholder
nix build .#gemma-container          # pre-baked image
```

### Strict variants (`*-model-strict`)

For every entry under `containers.models` there is a parallel
`containers.strictModels` entry exposed at the flake's package level as
`<key>-model-strict`. The strict derivation routes the model info
through `assertRealModelHash` first, which `throw`s with a reproduction
recipe whenever the registered hash is still the all-zeros placeholder.

Use the strict variant whenever an empty `$out/models` would be a latent
bug rather than an intentional dev shortcut — for example, release
container images, cached production paths, or as an acceptance check
after running `scripts/update-model-sha256.sh`. The default
`<key>-model` and `<key>-container` attributes still fall back to the
dev-mode stub on placeholder hashes; strict variants do not.

If you add a new model to `modelRegistry`, also wire it into both
`models` and `strictModels` in `nix/containers.nix`, and inherit it in
`flake.nix` alongside the existing entries.

### Why not `nix-prefetch-url`?

The Ollama registry is a multi-blob manifest protocol, not a single URL, so
`nix-prefetch-url` can't compute a useful hash by itself. The capture path
above is the canonical "let the fixed-output derivation tell you the real
hash" idiom - see the NixOS manual section on fixed-output derivations.

### Environments that cannot capture

CI sandboxes (including Claude Code web sessions) that lack outbound network
or can't run the Ollama daemon **must not** synthesize a value; they should
leave the placeholder in place and file a follow-up issue referencing
`nix/containers.nix`. The dev-mode branch in `createModelDerivation` keeps
the workspace lint-clean and the eval workflow functional (it pulls at
runtime via `ollama pull` with retry/timeout). See #240 for history.
