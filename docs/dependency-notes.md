# Dependency advisory assessment

Reviewed: 2026-11-07.

## deepmerge: GHSA-gcqx-w48m-w2h4

The frontend dependency audit reports three low-severity entries: `deepmerge`,
`@sveltejs/vite-plugin-svelte`, and `@sveltejs/vite-plugin-svelte-inspector`.
These entries trace to one underlying advisory, not three independent defects.

The locked build dependency chain is
`@sveltejs/vite-plugin-svelte@5.1.1` -> `deepmerge@4.3.1`.
The inspector's peer dependency on the Svelte plugin accounts for the other
reported entry. The advisory lists no patched version; the npm registry's latest
deepmerge release was still 4.3.1 on the review date.

### Scope and evidence

The installed plugin imports deepmerge in `src/utils/options.js`. Its
`mergeConfigs` function combines plugin defaults, local Svelte configuration,
inline plugin options, and Vite-derived options during configuration resolution.
Dockmin's Vite and Vitest configurations call `svelte()` with no inline options.
No application-request input path into these merges was identified.

Dockmin's production service runs a Rust executable and serves compiled static
frontend assets. The package script includes neither Node dependencies nor the
Svelte plugin. `npm ls --prefix web --omit=dev --all` lists only xterm and its fit
addon; `npm audit --prefix web --omit=dev` reported zero vulnerabilities.

This is a static reachability assessment, not an exploit reproduction or a
general guarantee that the dependency is safe. Build configuration is trusted
code; do not feed untrusted configuration objects into this merge path. Reassess
if the build starts accepting configuration from external sources or if the
deployment begins running Node build/development tooling.

### Decision

Retain the compatible locked build dependency for now, disclose the remaining
advisory, and review upstream releases for a supported fix. Do not suppress the
audit entry or apply an unverified override merely to obtain a zero count.
Before a release, rerun the full frontend audit and review this decision against
the current dependency chain. When a compatible fix becomes available, update
the lockfile and verify the frontend build and tests.

References:

- [GitHub advisory](https://github.com/advisories/GHSA-gcqx-w48m-w2h4)
- [Upstream issue](https://github.com/TehShrike/deepmerge/issues/273)
