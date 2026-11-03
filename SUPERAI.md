# Super AI

**Super AI** is a stronger, human-style computer opponent for skirmish, built on the
fork-only `superai/base` branch of VERA20k.

## What this is

Super AI is a **command-layer virtual player**. It observes the match through a
read-only view and issues the same command stream a human player would — it never
writes simulation state directly, so **determinism and replays stay intact**. The
stock computer opponents (**Easy**, **Normal**, **Hard**) are completely untouched:
an ordinary computer slot keeps the native AI. Super AI is strictly **opt-in** —
a match uses it only when you pick a Super AI row in the lobby. It lives only on
this fork's `superai/base` branch and does not go upstream into YuriPlanet/vera20k.

## Quick start

1. Clone the fork — the branch comes with the clone:

   ```sh
   git clone -b superai/base https://github.com/CCDawn/vera20k.git
   cd vera20k
   ```

2. Point the game at your install. Copy the configuration example, then edit
   `config.toml` and set `ra2_dir` to your own retail **Red Alert 2: Yuri's
   Revenge** installation directory. VERA20k plays the original game files,
   so you need your own copy:

   ```sh
   cp config.toml.example config.toml
   ```

3. Install the build prerequisites:

   - **Rust** [1.97](https://rustup.rs/) or later, installed with rustup.
   - **Build tools:** on Windows, the Visual Studio C++ build tools, which the
     Rust installer offers to install; on macOS, `xcode-select --install`;
     on Debian and Ubuntu,
     `sudo apt install build-essential libasound2-dev libxkbcommon-x11-0 pkg-config`.

4. Build and run:

   ```sh
   cargo run --release --bin vera20k
   ```

5. In the skirmish lobby, set a computer slot's difficulty dropdown to
   **Super AI** or **Super AI MAX**. That is the whole enablement — **no INI
   key and no command-line switch**. Every other slot, including the stock
   Easy/Normal/Hard rows, keeps native behavior.

## Difficulty tiers

| Tier | Strength | Lobby row |
|------|----------|-----------|
| **Fair** | Zero subsidies. Pure decisions only — the baseline for win-rate measurement. | — |
| **Brutal** | Fair + the native Hard AI's opening subsidy + a recurring +1,000 credits drip. | — |
| **Super** | Brutal + build times scaled ×0.5. | **Super AI** |
| **Unlimited** | Super + a per-tick wallet floor topped up to 1,000,000 credits. A spectacle tier for watching; do not cite it for strength claims. | **Super AI MAX** |

The lobby exposes only the **Super AI** (Super) and **Super AI MAX** (Unlimited)
rows. **Fair** and **Brutal** are benchmark-only tiers, used as measurement
baselines by the headless bench below.

## Headless win-rate benchmark

`sim-bench` plays the Super AI against the stock Hard computer without a window
and prints the win-loss record:

```sh
cargo run --release --bin sim-bench -- --scenario super --ra2-dir "C:/Westwood/RA2"
```

`--ra2-dir` may be omitted when the `RA2_DIR` environment variable names the
retail install. Useful flags:

- `--super-tier fair|brutal|super|unlimited` — which tier the Super house plays
  (default `fair`).
- `--games 10` — how many games the run plays.
- `--seed S` — the base RNG seed; game `i` runs with seed `S + i`, so a whole
  run replays identically from one number.
- `--map Death.mmx` — the skirmish map.
- `--super-country America` / `--builtin-country Russia` — the two houses' countries.
- `--builtin-difficulty hard` — the stock opponent's difficulty.

Each game prints a one-line result (`game 0 seed=0x…: winner=…`), and the run
ends with a summary such as `super record (Super): 9-1 over 10 game(s), 0 timeout(s)`.

## How it works

Picking a Super AI row turns that slot into a **command-driven house**. It still
receives the native Hard AI's opening subsidy, but the native AI's decision loops
stand down for that house, and the app merges the Super AI's orders into the
match's command stream on every tick. An ordinary computer slot is never touched.

The design notes live in `docs/superai.md`:

- **Positioning** — why the command layer, and what stays read-only.
- **Difficulty tiers** — how each tier's subsidies are applied.
- **In-game selection and the command-driven house** — what changes for a selected slot.
- **Determinism contract** — replays and saved games stay exact.
- **Engine hooks** — the small, documented fork surfaces inside the engine.
- **Upstream sync** — how this fork tracks upstream.

## Relationship to upstream

This fork periodically merges `origin/main` into `superai/base` (see
**Upstream sync** in `docs/superai.md`). Generic improvements are offered back
upstream as PRs. The fork's diff surface stays small and documented: this file,
`docs/superai.md`, `src/super_ai.rs` and a handful of documented engine hooks.
