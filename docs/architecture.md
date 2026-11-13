# Architecture

One daemon, one SQLite file, three capture boundaries.

```
agent harness (any) ──► model API
        │                    ▲
        │                    │  1. localhost proxy copies turns
        │  2. tool hook/wrapper emits a record per call
        │  3. working-tree diffs captured between turns
        ▼
   mayday daemon ──► timeline.db (SQLite)
                          │
                    replay UI: why did it do that
```

The rule for capture: passive by default. A harness that can't be hooked
still gets recorded at boundary 1, because every agent talks to a model API
and that traffic crosses a socket we can stand in front of.
