//! Match owner (F12 `MatchState`): the aggregate for everything scoped to a
//! single running match — the authoritative `SimRuntime`, the app-side input,
//! presentation, audio, and diagnostics owners, plus the scenario identity and
//! pacing facts that accompany them.
//!
//! App-side only: nothing here is serialized, hashed, or read by the
//! deterministic simulation except through `SimRuntime` itself.

use crate::map::basic::BasicSection;

pub(crate) struct MatchState {
    pub(crate) startup: super::startup::MatchStartup,
    /// Live session owner: gamemd's `SessionClass` at `0x00A8B238` (who
    /// holds `GameMode`, participants, local participant, host). Written by
    /// the shell, the launch path and the saved-game restore; read by the app
    /// loop only. Read `GameMode` through [`Self::session_mode`].
    pub(crate) session: super::session::MatchSession,
    /// The in-match connection wait and its dialog 0xEA
    /// (`Wait_For_Players 0x00648710`).
    pub(crate) network_wait: super::network_match::NetworkMatchWait,
    /// Input state for ExecuteDoList's blocking checksum-error message box.
    /// The suspended frame and mismatch remain owned by Simulation.
    pub(crate) network_failure: Option<super::network_failure::NetworkFailureModal>,
    /// VERA-only: a network frame failed before commit (`FrameAdvanceError`).
    /// The world keeps partial writes, so it never advances again; the same
    /// checksum box ends the match instead of resuming a suspended frame, with
    /// a body that calls it an error rather than a desync.
    pub(crate) frame_stopped: bool,
    /// Physical byte transport and packet progress for an installed match.
    /// Session owns the accepted participant roster and checksum history.
    pub(crate) live_network: Option<crate::net::live_commands::LiveCommandSession>,
    /// LAN visit transport before the match, then discovery metadata while the
    /// same sockets are owned by live_network. Session owns admitted endpoints.
    pub(crate) lan_service: Option<crate::app::lan_service::LanService>,
    pub(crate) sim_runtime: Option<crate::sim::runtime::SimRuntime>,
    /// Match input owner (F12): camera, zoom, cursor, keys, hotkeys.
    pub(crate) input: crate::app::input::state::MatchInputState,
    /// Match presentation owner (F12), part 1: per-match atlases + cursor.
    pub(crate) match_presentation: crate::app::presentation::state::MatchPresentationState,
    /// Per-match audio owner (F11): sound event queue + EVA latches; resets
    /// on every match install and on leaving a match for the shell.
    pub(crate) match_audio: crate::app::match_audio::MatchAudioState,
    /// App-owned diagnostic recording (F10) — never inside the simulation, so
    /// no load/install path can silently drop an unflushed segment.
    pub(crate) match_diagnostics: crate::app::match_diagnostics::MatchDiagnosticsState,
    /// Online match network quality record; wall-clock, never simulation input.
    pub(crate) network_quality: super::network_quality::NetworkQualityStats,
    /// Read-only process measurements; never inputs to the simulation.
    #[cfg(feature = "telemetry-dashboard")]
    pub(crate) telemetry_timing: super::telemetry::TickTimingRing,
    /// Whole-match 1 Hz timeline; recorded whether or not a dashboard client
    /// is attached, so a late client can backfill.
    #[cfg(feature = "telemetry-dashboard")]
    pub(crate) telemetry_history: super::telemetry::MatchHistory,
    /// Report-grade 5 s samples; each one renders the same report as the live
    /// view, including the time before a client attached.
    #[cfg(feature = "telemetry-dashboard")]
    pub(crate) report_history: super::telemetry::ReportHistory,
    #[cfg(feature = "telemetry-dashboard")]
    pub(crate) telemetry_tick_rate: super::telemetry::TickRateMeter,
    pub(crate) map_basic: BasicSection,
    /// Resolved Full_Init mission strings; retained independently of the
    /// current CSF and outside deterministic simulation state.
    pub(crate) campaign_names: crate::rules::campaign_loading::CampaignScenarioNames,
    /// Parsed `[Ranking]` values used by the campaign victory page.
    pub(crate) map_ranking: crate::map::ranking::RankingSection,
    /// Exact source whose bytes produced the active parsed map.
    pub(crate) loaded_map_source: Option<crate::map::source::LoadedMapSource>,
    /// Deterministic digest of the parsed source map INI. `None` only for
    /// generated/fallback worlds without an authoritative source-map payload.
    pub(crate) loaded_map_hash: Option<u64>,
    /// App-owned wall-clock House Vox drain. HouseState owns pending flags
    /// and SavourDelay; ScenarioSession retains the admitted result request.
    pub(crate) scenario_outcome:
        Option<crate::app::match_runtime::scenario_exit::ScenarioOutcomeVoiceWait>,
    /// Active running-scenario audio teardown. While present the tactical
    /// frame remains visible but simulation is frozen; its destination is
    /// committed only after the retail fade/voice-wait sequence completes.
    pub(crate) scenario_exit: Option<crate::app::match_runtime::scenario_exit::ScenarioExitCascade>,
    /// `g_bScenarioRestartRequested` `0x008B41C0`: B6 Restart raised it and
    /// the termination router consumes it before the next simulation frame.
    pub(crate) scenario_restart_requested: bool,
    /// Match elapsed wall time for the retail score screen. App-local and never
    /// serialized, hashed, or read by deterministic simulation.
    pub(crate) scenario_elapsed_clock: crate::app::match_runtime::frame_pacer::ScenarioElapsedClock,
    /// Config-sourced input delay — copied to each new Simulation instance at game start.
    pub(crate) configured_input_delay_ticks: u64,
    /// Seeded empty-map sandbox keeps full map visibility while still locking control.
    pub(crate) sandbox_full_visibility: bool,
    /// The debug pause (`J` hotkey): the simulation is frozen
    /// without an in-scenario modal. Every modal transition ends it.
    pub(crate) debug_pause: bool,
    /// Effective simulation ticks per second — controls game speed.
    /// Default follows retail/YR skirmish stored game speed 1.
    pub(crate) sim_speed_tps: u32,
    /// Fork-only Super-AI opponents (`docs/superai.md`): one command-layer
    /// driver per command-driven house of the installed runtime. Rebuilt on
    /// every runtime install (fresh launch and saved-game restore) from the
    /// houses' own state, so no driver ever outlives its match.
    pub(crate) super_players: Vec<crate::super_ai::SuperAi>,
    /// Config-sourced fork-only switch (`[gameplay] defense_ai`, default
    /// off): rebuild_super_players seats the houses it drives that face no
    /// other command-driven AI in the defense persona (`docs/defense-ai.md`).
    /// Copied at startup like configured_input_delay_ticks.
    pub(crate) defense_ai_enabled: bool,
}

impl MatchState {
    /// Rebuild the Super-AI drivers from the freshly installed runtime: one
    /// [`crate::super_ai::SuperAi`] per command-driven house. The tier rides
    /// on the house's own tier treatment — a build-time scale marks Super, a
    /// command-driven house without one is Brutal (`docs/superai.md`). With
    /// `defense_ai_enabled` the houses that face no other command-driven AI,
    /// and the houses of an explicitly classified defense map, adopt the
    /// defense persona (`docs/defense-ai.md`). Every install also lands the
    /// sim's defense-map form verdict for the defeat gate, switch off
    /// included.
    pub(crate) fn rebuild_super_players(&mut self) {
        let defense_ai = self.defense_ai_enabled;
        self.super_players = self
            .sim_runtime
            .as_mut()
            .map(|rt| {
                rebuild_super_players_for(&mut rt.simulation, &rt.resources.rules, defense_ai)
            })
            .unwrap_or_default();
    }
    /// Shared ScenarioPause684060 audio hold, independent of menu/debug pause.
    /// Main_Tick55D82C also gates offline world admission on its counter.
    pub(crate) fn presentation_paused(&self, screen: &crate::ui::game_screen::GameScreen) -> bool {
        presentation_pause_for_screen(
            screen,
            self.paused(),
            self.sim_runtime.as_ref().map(|rt| rt.simulation.session()),
        )
    }
    /// Native elapsed-clock intent excludes the relay hold and network
    /// menus: those menus never stop the score clock. Audio still combines
    /// every physical pause through presentation_paused.
    pub(crate) fn elapsed_clock_paused(&self, screen: &crate::ui::game_screen::GameScreen) -> bool {
        elapsed_clock_pause_for_screen(
            screen,
            self.session_mode(),
            self.match_presentation.in_game_menu.is_open(),
            self.match_presentation
                .radar_anim
                .as_ref()
                .is_some_and(|radar| radar.movie_pause()),
            self.sim_runtime
                .as_ref()
                .is_some_and(|runtime| runtime.simulation.session().scenario_paused()),
            self.match_presentation.commander_feedback.result_waits(),
        )
    }

    pub(crate) fn paused(&self) -> bool {
        self.local_paused() || self.multiplayer_paused()
    }

    pub(crate) fn local_paused(&self) -> bool {
        self.match_presentation.in_game_menu.is_open()
            || self.debug_pause
            || self
                .match_presentation
                .radar_anim
                .as_ref()
                .is_some_and(|radar| radar.movie_pause())
    }

    /// The relay's marker applies after the entire barrier frame, including a
    /// retained post-Logic transaction. This is a view of network authority,
    /// not an app-owned pause flag or a Scenario pause credit.
    pub(crate) fn multiplayer_paused(&self) -> bool {
        self.live_network
            .as_ref()
            .and_then(|live| live.multiplayer_pause())
            .zip(self.sim_runtime.as_ref())
            .is_some_and(|(pause, runtime)| {
                multiplayer_pause_reached(
                    pause,
                    runtime.simulation.session().binary_frame(),
                    runtime.simulation.pending_network_wait(),
                )
            })
    }

    pub(crate) fn sync_multiplayer_pause_clock(&mut self, wall_ms: u64) {
        let paused = self.multiplayer_paused();
        self.scenario_elapsed_clock
            .set_multiplayer_paused(paused, wall_ms);
    }

    /// Main_Tick (`0x0055D360`) is not running. A LAN/WOL modal's pump
    /// `0x00623120` keeps calling it in GameMode 3/4, so what Main_Tick
    /// drives keeps going under the dialog: `MessageListClass::Manage`
    /// (`0x005D4430`, its only caller) ages the rows. Debug pause remains a
    /// local diagnostic gate; native offline Scenario62C gates all its holds.
    pub(crate) fn main_tick_frozen(&self) -> bool {
        self.sim_runtime
            .as_ref()
            .is_some_and(|rt| rt.simulation.pending_network_failure().is_some())
            || self.debug_pause
            || self.multiplayer_paused()
            || super::scenario_pause::blocks_main_tick(
                self.session_mode(),
                self.sim_runtime
                    .as_ref()
                    .is_some_and(|rt| rt.simulation.session().scenario_paused()),
            )
            || (self.match_presentation.in_game_menu.is_open() && !self.session_mode().is_network())
    }

    /// `GameMode` (`0x00A8B238`) from whichever side holds it: the installed
    /// world's saved ScenarioSession from install until the shell writes it
    /// again, otherwise the shell (see `MatchSession`).
    pub(crate) fn session_mode(&self) -> super::session::SessionMode {
        held_session_mode(
            &self.session,
            self.sim_runtime.as_ref().map(|rt| rt.simulation.session()),
        )
    }

    /// The live scenario's pinned player, including a successfully restored save.
    pub(crate) fn local_player_owner(&self) -> Option<&str> {
        self.sim_runtime
            .as_ref()
            .and_then(|runtime| scenario_local_owner(&runtime.simulation))
    }
}

fn multiplayer_pause_reached(
    pause: crate::net::live_commands::MultiplayerPauseStatus,
    next_frame: u32,
    transaction_pending: bool,
) -> bool {
    pause.available && pause.paused && next_frame > pause.frame && !transaction_pending
}

/// `result_waits` is VERA's: a published result the exit router leaves
/// waiting for a commander clip. Native routes a result after the frame that
/// publishes it, so its score time ends on that frame; the clip's own pause
/// and resume must not restart the clock.
fn elapsed_clock_pause_for_screen(
    screen: &crate::ui::game_screen::GameScreen,
    mode: super::session::SessionMode,
    menu_open: bool,
    movie_paused: bool,
    scenario_paused: bool,
    result_waits: bool,
) -> bool {
    matches!(screen, crate::ui::game_screen::GameScreen::InGame)
        && (movie_paused
            || result_waits
            || (menu_open && !mode.is_network())
            || super::scenario_pause::blocks_main_tick(mode, scenario_paused))
}

/// A retained runtime can outlive its tactical screen. This is a physical
/// audio/presentation query; it never retires saved simulation pause credits.
fn presentation_pause_for_screen(
    screen: &crate::ui::game_screen::GameScreen,
    local_pause: bool,
    scenario: Option<&crate::sim::scenario_session::ScenarioSession>,
) -> bool {
    matches!(screen, crate::ui::game_screen::GameScreen::InGame)
        && (local_pause || scenario.is_some_and(|session| session.scenario_paused()))
}

fn held_session_mode(
    session: &super::session::MatchSession,
    world: Option<&crate::sim::scenario_session::ScenarioSession>,
) -> super::session::SessionMode {
    session.shell_mode().unwrap_or_else(|| {
        world
            .expect("only an installed world holds GameMode")
            .game_mode()
    })
}

#[cfg(test)]
mod tests {
    use super::{
        elapsed_clock_pause_for_screen, held_session_mode, multiplayer_pause_reached,
        presentation_pause_for_screen, rebuild_super_players_for,
    };
    use crate::app::match_runtime::session::{MatchSession, NetworkReturnContext, SessionMode};
    use crate::sim::world::Simulation;
    use crate::ui::game_screen::GameScreen;

    #[test]
    fn shared_pause_waits_for_the_complete_barrier_and_survives_pending_resume() {
        let mut pause = crate::net::live_commands::MultiplayerPauseStatus {
            available: true,
            paused: true,
            frame: 42,
            epoch: 1,
            pending: false,
        };
        assert!(!multiplayer_pause_reached(pause, 42, false));
        assert!(!multiplayer_pause_reached(pause, 43, true));
        assert!(multiplayer_pause_reached(pause, 43, false));
        pause.pending = true;
        assert!(multiplayer_pause_reached(pause, 43, false));
        pause.paused = false;
        assert!(!multiplayer_pause_reached(pause, 43, false));
    }

    #[test]
    fn game_mode_reads_from_its_one_holder() {
        let mut session = MatchSession::default();
        let mut world = Simulation::with_seed(0);
        world.install_session_mode(SessionMode::Skirmish);
        // Process start: the shell holds 0 even beside a retained world.
        assert_eq!(held_session_mode(&session, None), SessionMode::Campaign);
        assert_eq!(
            held_session_mode(&session, Some(world.session())),
            SessionMode::Campaign
        );
        session.begin_offline(SessionMode::Skirmish, None);
        session.hand_mode_to_world(world.session().game_mode());
        assert_eq!(
            held_session_mode(&session, Some(world.session())),
            SessionMode::Skirmish
        );
        // A shell write after the match takes it back from the finished world.
        session.enter_network(NetworkReturnContext::Wol);
        assert_eq!(
            held_session_mode(&session, Some(world.session())),
            SessionMode::Wol
        );
    }

    /// A result the exit router leaves waiting for a commander clip keeps the
    /// score clock stopped, although the clip itself does not pause the match.
    #[test]
    fn a_result_waiting_for_a_clip_keeps_the_score_clock_stopped() {
        let paused = |result_waits| {
            elapsed_clock_pause_for_screen(
                &GameScreen::InGame,
                SessionMode::Skirmish,
                false,
                false,
                false,
                result_waits,
            )
        };
        assert!(!paused(false));
        assert!(paused(true));
    }

    #[test]
    fn retained_pause_does_not_leak_into_result_or_shell_audio() {
        assert!(elapsed_clock_pause_for_screen(
            &GameScreen::InGame,
            SessionMode::Skirmish,
            true,
            false,
            false,
            false
        ));
        assert!(!elapsed_clock_pause_for_screen(
            &GameScreen::InGame,
            SessionMode::Wol,
            true,
            false,
            true,
            false
        ));
        assert!(elapsed_clock_pause_for_screen(
            &GameScreen::InGame,
            SessionMode::Wol,
            true,
            true,
            false,
            false
        ));
        let mut sim = Simulation::with_seed(0);
        sim.publish_scenario_started();
        sim.pause_scenario_for_test(2);
        assert!(presentation_pause_for_screen(
            &GameScreen::InGame,
            false,
            Some(sim.session())
        ));
        // Score entry retains ScenarioStarted until Continue; it is not the
        // presentation-lifetime boundary. Shell return retains the runtime too.
        for screen in [
            GameScreen::MissionResult {
                title: String::new(),
                detail: String::new(),
            },
            GameScreen::MainMenu,
            GameScreen::Loading,
        ] {
            assert!(
                !presentation_pause_for_screen(&screen, false, Some(sim.session())),
                "{screen:?}"
            );
        }
        // A failed load returning to the current world must retain the hold.
        assert!(sim.session().scenario_pause_timed());
        assert!(presentation_pause_for_screen(
            &GameScreen::InGame,
            false,
            Some(sim.session())
        ));
        sim.expire_scenario_pause_for_test();
        assert!(!presentation_pause_for_screen(
            &GameScreen::InGame,
            false,
            Some(sim.session())
        ));
        assert!(presentation_pause_for_screen(
            &GameScreen::InGame,
            true,
            Some(sim.session())
        ));
    }

    /// The fork-only defense seating: only a command-driven house whose
    /// enemies hold no other command-driven AI adopts the persona, and only
    /// when the config switch asks. Allied command-driven houses are not
    /// each other's opponents; a human never drives a driver.
    #[test]
    fn defense_flag_seats_only_houses_without_a_command_ai_opponent() {
        use crate::sim::house_state::HouseState;
        use crate::super_ai::DefenseStance;
        use crate::super_ai::SuperAiDifficulty;

        let mut house = |sim: &mut Simulation, name: &str, human: bool| {
            let name = sim.intern(name);
            let country = sim.intern("Americans");
            let mut state = HouseState::new(name, 0, Some(country), human, 0, 10);
            if !human {
                state.make_command_driven(SuperAiDifficulty::Brutal);
            }
            sim.houses_mut_for_test().register(state);
            name
        };
        let mut sim = Simulation::with_seed(1);
        house(&mut sim, "Computer1", false);
        house(&mut sim, "Computer2", false);
        let rules = crate::sim::runtime::SimResources::empty().rules;
        // Switch off: the battle persona stays, rivals or not.
        let players = rebuild_super_players_for(&mut sim, &rules, false);
        assert_eq!(players.len(), 2);
        assert!(players.iter().all(|ai| ai.defense_stance().is_none()));
        // Switch on: each AI faces a rival command-driven AI, so neither
        // seats.
        let players = rebuild_super_players_for(&mut sim, &rules, true);
        assert!(players.iter().all(|ai| ai.defense_stance().is_none()));
        // Allied teammates are not opponents: both seat.
        for (asker, other) in [("Computer1", "Computer2"), ("Computer2", "Computer1")] {
            sim.house_alliances_mut_for_test()
                .entry(asker.to_ascii_uppercase())
                .or_default()
                .insert(other.to_ascii_uppercase());
        }
        let players = rebuild_super_players_for(&mut sim, &rules, true);
        assert_eq!(
            players
                .iter()
                .map(|ai| ai.defense_stance())
                .collect::<Vec<_>>(),
            [Some(DefenseStance::IndependentZone); 2]
        );

        // A lone command-driven AI against a human: its pressure is the
        // map's waves, so it seats; the human drives nothing.
        let mut sim = Simulation::with_seed(2);
        let ai = house(&mut sim, "Computer1", false);
        house(&mut sim, "Player", true);
        let players = rebuild_super_players_for(&mut sim, &rules, true);
        assert_eq!(players.len(), 1);
        assert_eq!(players[0].house(), ai);
        assert_eq!(
            players[0].defense_stance(),
            Some(DefenseStance::IndependentZone)
        );
    }
}

/// Native current House A83D4C is saved67F802, loaded67F9F3 and swizzled67FA0E.
/// Do not retain a separate app owner across Simulation replacement or infer it
/// from selection/human flags. Save validation owns reference validity.
/// Evidence: docs/research/PHASE3_CURRENT_HOUSE_IDENTITY_GHIDRA_REPORT.md.
pub(crate) fn scenario_local_owner(simulation: &crate::sim::world::Simulation) -> Option<&str> {
    simulation
        .session()
        .current_house()
        .map(|owner| simulation.interner().resolve(owner))
}

/// The Super-AI driver assembly [`MatchState::rebuild_super_players`]
/// delegates to, split from the MatchState owner so a test can seat it on a
/// bare Simulation.
fn rebuild_super_players_for(
    simulation: &mut crate::sim::world::Simulation,
    rules: &crate::rules::ruleset::RuleSet,
    defense_ai: bool,
) -> Vec<crate::super_ai::SuperAi> {
    // The driven-seat roster the classification read takes: every human seat
    // plus every command-driven AI seat the map leaves in play — a
    // multiplay-passive house is excluded, the persona seating's own filter
    // (`command_driven() && !multiplay_passive()`), because a survival
    // map's wave seat is exactly that: an AI-driven house the map holds
    // passive so its triggers alone speak for it (DefendLiberty's
    // `<Player @ E>` host). A wave team whose GetHouse owner resolves to a
    // house outside the driven seats — an unseated slot or a passive wave
    // host — belongs to the defense map's own machinery, not a battle
    // opponent.
    let driven: Vec<crate::sim::intern::InternedId> = simulation
        .houses()
        .iter()
        .filter(|(_, state)| {
            state.is_human() || (state.command_driven() && !state.multiplay_passive())
        })
        .map(|(house, _)| *house)
        .collect();
    // Fork-only defense-map classification (`docs/defense-ai.md`): the sim's
    // single-entry read over the armed trigger instances, pure and run on
    // every install. Its verdict gates the defeat gate's future-reinforcement
    // hold (`world::house_defeat`), so it also runs with the switch off — an
    // ordinary match only gains that field write. One INFO line per armed
    // install carries the verdict and the armed spawn-trigger count the
    // verdict read.
    let (defense_map, spawn_triggers) = simulation.defense_map_classification(&driven, rules);
    simulation.set_defense_map_form(defense_map);
    if defense_ai {
        log::info!(
            "[DEF-AI] map classified: defense-map={defense_map} (spawn-triggers={spawn_triggers})"
        );
    }
    // Each house carries its launch tier mark; saves from before the mark
    // fall back to the build-scale heuristic (a scale marks Super, a
    // scale-less command-driven house is Brutal).
    let command_driven: Vec<(
        crate::sim::intern::InternedId,
        crate::super_ai::SuperAiDifficulty,
    )> = simulation
        .houses()
        .iter()
        .filter(|(_, state)| {
            state.command_driven() && !state.multiplay_passive() && !state.is_human()
        })
        .map(|(house, state)| {
            (
                *house,
                state
                    .super_tier()
                    .unwrap_or(if state.native_build_time_scale().is_some() {
                        crate::super_ai::SuperAiDifficulty::Super
                    } else {
                        crate::super_ai::SuperAiDifficulty::Brutal
                    }),
            )
        })
        .collect();
    let alliances = simulation.house_alliances().as_map();
    // Mode B's seat-time read (`docs/defense-ai.md`): a classified defense
    // map whose shared lose-chain objective set holds between one and
    // [`crate::super_ai::DEFENSE_SHARED_OBJECTIVE_CAP`] standing objects
    // arms the full-map cooperative stance for every defense seat — the
    // same read the persona's perception runs, taken once at install.
    // Everything else plays mode A.
    let stance = if defense_map {
        let shared = crate::super_ai::defense_shared_target_count(simulation);
        if (1..=crate::super_ai::DEFENSE_SHARED_OBJECTIVE_CAP).contains(&shared) {
            crate::super_ai::DefenseStance::SharedObjective
        } else {
            crate::super_ai::DefenseStance::IndependentZone
        }
    } else {
        crate::super_ai::DefenseStance::IndependentZone
    };
    command_driven
        .iter()
        .copied()
        .map(|(house, tier)| {
            let mut ai = crate::super_ai::SuperAi::new(house, tier);
            // Fork-only: with the config switch on, a house whose enemies
            // hold no other command-driven AI is pressed by map waves rather
            // than an AI opponent, so it plays the defense persona. The same
            // both-direction ally read super_ai's own is_enemy makes
            // (in-game diplomacy can be one-way); candidates above are
            // already neither human nor MultiplayPassive.
            let faces_command_ai = command_driven.iter().copied().any(|(other, _)| {
                other != house
                    && !crate::map::houses::is_allied_with(
                        alliances,
                        simulation.resolve(house),
                        simulation.resolve(other),
                    )
                    && !crate::map::houses::is_allied_with(
                        alliances,
                        simulation.resolve(other),
                        simulation.resolve(house),
                    )
            });
            // A classified defense map seats its houses whatever the
            // enemy test reads (allied AI co-defenders face each other on
            // it), at the stance chosen above.
            if defense_ai && (defense_map || !faces_command_ai) {
                ai.enable_defense_personality(stance);
            }
            ai
        })
        .collect()
}
