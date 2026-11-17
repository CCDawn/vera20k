# vera20k-superai

VERA20k's fork-original Super AI: a deterministic decision layer that plays
one house through the same command stream a human client sends. Not a gamemd
port; see `docs/superai.md` (in the engine checkout) for the design ledger.

## Integration

The engine core (`vera20k-core`) carries no Super AI knowledge; this plugin
depends on it, never the reverse. Add it as a git dependency that mirrors
this repository:

```toml
vera20k-superai = { git = "https://github.com/CCDawn/vera20k-superai" }
```

## Stable interface

- `SuperAi::new(house: InternedId, difficulty: SuperAiDifficulty)` — attach
  one driver to a command-driven house.
- `SuperAi::orders(&SimRuntime, tick) -> Vec<CommandEnvelope>` — per-tick
  decision output, a pure function of the simulation view, the tick and the
  driver's pinned state (determinism contract in the crate docs). Feeding
  the envelopes to the simulation never mutates the runtime.
- `SuperAiDifficulty::into` → the engine's `DriverPolicy` (`From`) and
  `infer_tier(&DriverPolicy)` — the difficulty ladder maps onto the launch
  treatment a command-driven house carries and reads back.

Difficulty ladder, one line: `Fair` (no subsidy) → `Brutal` (Hard-row
opening grant + credit drip) → `Super` (halved build times) → `Unlimited`
(wallet floor `UNLIMITED_WALLET_FLOOR` every tick).
