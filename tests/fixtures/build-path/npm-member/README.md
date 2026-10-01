# demo-npm-member

Fixture npm member for the jumbo-publish build-before-pack check. Its
`files` list ships `dist` only, so a tarball packed WITHOUT building is a
source-only stub (package.json + README) — exactly the defect the check
pins: the npm publication path must run the package's standard build
before `npm pack`.
