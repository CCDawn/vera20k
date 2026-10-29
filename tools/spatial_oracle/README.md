# Spatial native comparisons

Each top-level `<name>.py` runs retail `gamemd.exe` code under Unicorn and checks
or writes `<name>.json` with a `<name>.meta.json` sidecar. See the
[shared native runner](../native_oracle.md) for setup, `--check`/`--write` and
evidence limits. Subdirectories hold larger packets with their own README
(for example [anytown_damage](anytown_damage/README.md) and
[fv_cell_attack](fv_cell_attack/README.md)).

## Shared fixture owners

Most producers build on a few fixtures. Extend these instead of copying their
setup.

| Module | Provides | Importers |
| --- | --- | ---: |
| `map_queries` | Sparse cell-table state for the original map lookup, normalization and playfield bodies; `dwords`, `packed`, `TABLE`/`EMPTY_TABLE`/`GLOBAL_TABLE` | ~220 |
| `building_body_rules` | BuildingType body readers over supplied INI lookups; `SP`, `INI`, `RULES`, `TYPE`, `Fixture` | ~170 |
| `unit_source_scatter` | Source-aware Unit Scatter map and actor; `SCENARIO`, `CELLS`, `make_source_fixture` | ~40 |
| `refinery_dock` | Refinery/depot dock fixture; `make_dock_fixture`, `cell`, `HOUSE`, `BLD` | ~35 |
| `building_construction` | `JoinedFixture` and `joined_inputs`, the joined building world reused by building death, sale, repair, capture and factory output | ~30 |
| `anim_bouncer_launch` | Anim constructor through Bouncer; `constructor_state` decodes Anim state for construction, deploy, death, shrapnel, FV and AnyTown snapshots | ~20 |
| `guarded_arena` | `Arena`, guarded supplied malloc/free for fixtures that allocate | 2 |

A change to a shared owner changes every importer's inputs. Run the importers'
`--check` before publishing, and list affected corpora in the PR.

## Finding producers and consumers

```sh
git grep -l 'spatial_oracle.map_queries' -- tools   # Python importers of a fixture
git grep -n 'spatial_oracle/<name>.json' -- src     # Rust tests reading a corpus
python -m tools.spatial_oracle.<name> --check       # replay; the sidecar names the command
```

A harness stays only while a Rust test reads its output (AGENTS.md). Delete a
producer and its corpus together once its last consumer is gone.
