# waku-protocol

`waku-protocol` is Waku's versioned, transport-neutral contract. It contains
the Serde wire messages and shared data models, but no database, provider,
filesystem, Git, attachment, or socket implementation.

Generate the checked-in TypeScript contract with:

```sh
bun run protocol:generate
```

CI and local validation can detect stale generated files with
`bun run protocol:check`. Large Rust integers are emitted as TypeScript
`number` because the JSON transport cannot carry JavaScript bigint literals.
