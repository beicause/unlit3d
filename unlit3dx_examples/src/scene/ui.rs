//! The control panel: the scene's UI, drawn over the frame.
//!
//! The panel lives in the gameplay world with the rest of the scene's objects,
//! and the render world draws it. A [`UiPanel`] can only be built where its
//! egui context runs, so the gameplay world lays the panel out on its own tick
//! and publishes the frame; the render world's [`UiSource`] draws whatever the
//! slots hold.
//!
//! Everything the panel decides is decided here, beside the objects it acts
//! on: the drop and clear buttons call the scene's own request helpers, and
//! the transport buttons reach the audio world through the sender it was
//! spawned with.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use unlit_ecs::World;
use unlit3d::prelude::*;
use unlit3d::ui::{UiPanel, UiSource, egui};
use unlit3dx::prelude::*;

use super::music::sequence_midi;
use super::{BGM_TRACK, PanelSlots, SEQUENCE_TRACK, request_clear_in, request_drop_in};

/// The panel, living on its own entity in the gameplay world.
///
/// The entity carries the [`UiPanel`] that draws it, so the panel is mounted by
/// spawning one entity rather than by editing either world's setup. The
/// [`UiPanelHost`](unlit3dx::ui::UiPanelHost) that lays it out is an entity of
/// its own, spawned beside the panel it serves.
pub struct Controls {
    /// The audio world's command sender, when the scene has tracks.
    audio: Option<JobSender>,
}

/// A job that asks the audio world how many times `track` has played to
/// its end, answering zero for a track the world does not carry.
///
/// The count is what carries the event: a reader that remembers the count
/// it last saw sees every end, even when it asked nothing in between.
fn track_ends_job(track: &str) -> Job<u64> {
    let track = track.to_owned();
    Box::new(move |world: &mut World| {
        world
            .root_view()
            .query::<&MidiPlayer>()
            .find(|(_, player)| player.name() == track.as_str())
            .map_or(0, |(_, player)| player.ends())
    })
}

/// What one of the panel's two track rows shows.
///
/// A panel must not keep this in a closure local. egui may run a panel
/// more than once in one frame, so whatever a panel advances belongs in
/// a sibling component; the audio world that owns the real transport
/// lives on its own thread, so the panel records the intent here instead
/// of reading the tracks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackStatus {
    /// Nothing was ever put on the track, so its row offers a start.
    Idle,
    /// The track is playing.
    Playing,
    /// The track was paused from the panel.
    Paused,
}

/// The state one track row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackRow {
    /// What the row shows.
    status: TrackStatus,
    /// How many times the audio world had reported this track ended when
    /// the row last looked, so a fresh count is what resets it.
    ends: u64,
}

impl TrackRow {
    /// The row after the audio world reports `ends` ends for its track: a
    /// one-shot track that played to its end since the row last looked is
    /// back at its start, ready to play again.
    ///
    /// The count is what carries the event: a row that remembered a flag
    /// could miss an end that came and went between two frames.
    fn observed(self, ends: u64) -> Self {
        if ends > self.ends {
            Self {
                status: TrackStatus::Idle,
                ends,
            }
        } else {
            self
        }
    }
}

/// The counts the panel's rows read.
///
/// The audio world lives on its own thread, so the panel cannot read its
/// tracks: a watcher task asks about each through [`track_ends_job`] and
/// lands the answer here, and the panel reads the newest counts. The
/// panel keeps nothing in flight and asks the audio world nothing, so a
/// panel egui runs any number of times in a frame reads the same counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackEnds {
    /// How many ends the audio world last counted for the background.
    background: u64,
    /// How many ends it last counted for the sequence.
    sequence: u64,
}

impl TrackEnds {
    /// Nothing counted yet.
    const START: Self = Self {
        background: 0,
        sequence: 0,
    };
}

/// The tracks the panel transports, in the order its rows show them.
const TRACKS: [&str; 2] = [BGM_TRACK, SEQUENCE_TRACK];

/// Where the watcher task lands the counts the panel reads.
type TrackEndCounts = Rc<RefCell<TrackEnds>>;

/// Keep asking the audio world how many times each track played to its
/// end, one answer at a time, until the world stops answering.
///
/// A query is awaited rather than polled, so a count lands in `counts`
/// between two frames and the query after it goes out then: the audio
/// world never holds more than one query per watch, and a row that reads
/// `counts` on any frame sees the newest answer. A dropped query reports
/// the last count it gave and is asked again, so a world that restarts
/// does not end the watch.
///
/// The render world runs it as one of its own tasks, so it lasts as long
/// as the world rather than as long as the mount that started it, and no
/// caller keeps an executor beside the world for it.
async fn watch_track_ends(audio: JobSender, counts: TrackEndCounts) {
    loop {
        for (index, track) in TRACKS.iter().enumerate() {
            let count = match audio.request(track_ends_job(track)).await {
                Ok(count) => count,
                Err(error) => {
                    log::warn!("the audio world dropped a track-ends query ({error:?})");
                    continue;
                }
            };
            let mut ends = counts.borrow_mut();
            match index {
                0 => ends.background = count,
                _ => ends.sequence = count,
            }
        }
    }
}

/// The state the panel's two track rows show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackRows {
    /// What the background row shows.
    background: TrackRow,
    /// What the sequence row shows.
    sequence: TrackRow,
}

impl TrackRows {
    /// What both rows show before anyone touches them: the background
    /// loop starts itself, and the one-shot sequence waits for its row.
    const START: Self = Self {
        background: TrackRow {
            status: TrackStatus::Playing,
            ends: 0,
        },
        sequence: TrackRow {
            status: TrackStatus::Idle,
            ends: 0,
        },
    };
}

/// One button a track row can be drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackButton {
    /// Starts the track from its beginning, once: an idle row's only
    /// button, and the only way the one-shot sequence ever gets going.
    Play,
    /// Pauses a playing track.
    Pause,
    /// Resumes a paused track where it stopped.
    Resume,
    /// Returns the track to its start, keeping its pause.
    Rewind,
}

impl TrackButton {
    /// The label the button is painted with, naming what pressing it does
    /// next.
    fn label(self) -> &'static str {
        match self {
            Self::Play => "Play",
            Self::Pause => "Pause",
            Self::Resume => "Resume",
            Self::Rewind => "Rewind",
        }
    }
}

impl TrackStatus {
    /// The buttons a row in this state shows, in the order it shows them.
    fn buttons(self) -> &'static [TrackButton] {
        match self {
            Self::Idle => &[TrackButton::Play],
            Self::Playing => &[TrackButton::Pause, TrackButton::Rewind],
            Self::Paused => &[TrackButton::Resume, TrackButton::Rewind],
        }
    }

    /// The state a row showing this moves to when `button` is pressed.
    ///
    /// A rewind leaves the pause as it stands: the track restarts where it
    /// stopped, and a paused one stays silent.
    fn pressed(self, button: TrackButton) -> Self {
        match (self, button) {
            (Self::Idle, TrackButton::Play) | (Self::Paused, TrackButton::Resume) => Self::Playing,
            (Self::Playing, TrackButton::Pause) => Self::Paused,
            _ => self,
        }
    }
}

/// One track's transport: its name, then the buttons its state calls for.
///
/// The row names the track so the buttons need no suffix of their own. An
/// idle row starts the track with the file only the sequence has. A
/// one-shot track that played to its end is idle again, so its row offers
/// the same Play: `ends` is the count that tells the row that happened.
/// It answers the row to store.
fn track_controls(
    ui: &mut egui::Ui,
    audio: &JobSender,
    name: &str,
    row: TrackRow,
    file: Option<fn() -> Arc<MidiFile>>,
    ends: u64,
) -> TrackRow {
    let mut row = row.observed(ends);
    ui.horizontal(|ui| {
        ui.label(match name {
            SEQUENCE_TRACK => "MIDI sequence",
            _ => "Background music",
        });
        for button in row.status.buttons() {
            if !ui.button(button.label()).clicked() {
                continue;
            }
            let job = match button {
                TrackButton::Play => file.map(|file| play_track_job(name, file(), false)),
                TrackButton::Pause => Some(pause_track_job(name, true)),
                TrackButton::Resume => Some(pause_track_job(name, false)),
                TrackButton::Rewind => Some(rewind_track_job(name)),
            };
            if let Some(job) = job {
                audio.send(job).ok();
                row.status = row.status.pressed(*button);
            }
        }
    });
    row
}

impl Controls {
    /// The panel for a scene whose audio world `audio` reaches.
    ///
    /// The gameplay world needs no handle: the panel lives in it, so its own
    /// buttons call the scene's request helpers directly.
    #[must_use]
    pub fn new(audio: Option<JobSender>) -> Self {
        Self { audio }
    }

    /// Mount the panel: one entity carrying the source that runs it, the
    /// panel itself, and the rows it shows.
    ///
    /// The panel reads its input from the gameplay world's own buffer, which
    /// the world subscribes when it is set up and dispatches on its tick, so
    /// nothing input-related is spawned here: the events the panel reacts to
    /// are the ones the rest of this world saw on the same tick.
    ///
    /// The background loop starts itself once the audio world can sound; the
    /// one-shot sequence waits for its row's Play button and plays once per
    /// press. Each track then pauses, resumes and rewinds on its own, and the
    /// shapes drop or clear without touching the music. A world with no
    /// synthesizer has nothing to transport, so its panel shows the shapes
    /// alone.
    ///
    /// The rows live in a component on the same entity as the panel, so the
    /// callback owns no state of its own; the end counts the watcher lands are
    /// shared with it rather than stored, because the watcher cannot borrow
    /// the world it runs beside.
    pub fn mount(self, world: &mut World) {
        let watcher_audio = self.audio.clone();
        let counts: TrackEndCounts = Rc::new(RefCell::new(TrackEnds::START));
        let panel_counts = Rc::clone(&counts);
        // The panel's callback reads its own rows, so the entity has to be
        // known before the panel is built: reserve it, then spawn the whole
        // object onto the reservation.
        let entity = world.reserve_entity();
        world.root_view_mut().spawn_at(
            entity,
            (
                self,
                TrackRows::START,
                UiPanel::new(move |world, entity, ui| {
                    // The panel reads its own handle: the object on this entity
                    // owns it, so the callback carries nothing but the counts.
                    let Some(audio) = world.get::<Controls>(entity).map(|c| c.audio.clone()) else {
                        return;
                    };
                    egui::Window::new("Controls").show(ui.ctx(), |ui| {
                        if let Some(audio) = &audio {
                            // The counts first, then the rows: a row is put back by
                            // the end the watcher has already picked up.
                            let counts = *panel_counts.borrow();
                            let shown = world
                                .get::<TrackRows>(entity)
                                .map_or(TrackRows::START, |rows| *rows);
                            let now = TrackRows {
                                background: track_controls(
                                    ui,
                                    audio,
                                    BGM_TRACK,
                                    shown.background,
                                    None,
                                    counts.background,
                                ),
                                sequence: track_controls(
                                    ui,
                                    audio,
                                    SEQUENCE_TRACK,
                                    shown.sequence,
                                    Some(sequence_midi),
                                    counts.sequence,
                                ),
                            };
                            if now != shown {
                                let _ = world.with_mut::<TrackRows, _>(entity, |rows| *rows = now);
                            }
                            ui.separator();
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Drop the next shape").clicked() {
                                request_drop_in(&world);
                            }
                            if ui.button("Clear the shapes").clicked() {
                                request_clear_in(&world);
                            }
                        });
                    });
                }),
            ),
        );
        if let Some(audio) = watcher_audio {
            // The watcher is the gameplay world's own task: this job runs on the
            // frame that drains it, and the pump before the dispatch drives it.
            let task = watch_track_ends(audio, counts);
            if !LocalTasks::spawn(world, task) {
                log::error!("the gameplay world has no task set for its track watcher");
            }
        }
    }
}

/// A job that gives the render world the source drawing this scene's panel.
///
/// The panel itself is not here: it lives in the gameplay world, which lays it
/// out and publishes it. This world draws whatever the slots hold, so the two
/// worlds share the slots and nothing else.
pub fn control_panel_job(panels: PanelSlots) -> Job<()> {
    Box::new(move |world: &mut World| {
        let mut source = UiSource::new();
        source.draw_published(panels.frames, panels.screen);
        world.root_view_mut().spawn((Source::new(source),));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row offers only what its track can do: an idle track offers a
    /// start, and a started one never offers another — the sequence is one
    /// shot, so a second play would restart it from the top mid-phrase.
    #[test]
    fn a_track_row_offers_the_actions_its_state_allows() {
        assert_eq!(TrackStatus::Idle.buttons(), &[TrackButton::Play]);
        assert_eq!(
            TrackStatus::Playing.buttons(),
            &[TrackButton::Pause, TrackButton::Rewind]
        );
        assert_eq!(
            TrackStatus::Paused.buttons(),
            &[TrackButton::Resume, TrackButton::Rewind]
        );
    }

    /// Pressing a row's buttons walks it the way the audio world does: play
    /// starts it, pause holds it, resume lets it go on, and a rewind
    /// restarts it without changing whether it is paused.
    #[test]
    fn a_track_row_steps_the_way_the_transport_does() {
        let playing = TrackStatus::Idle.pressed(TrackButton::Play);
        assert_eq!(playing, TrackStatus::Playing);
        // Only an idle row has a play button, and it never returns to one.
        assert_eq!(playing.pressed(TrackButton::Play), TrackStatus::Playing);
        let paused = playing.pressed(TrackButton::Pause);
        assert_eq!(paused, TrackStatus::Paused);
        assert_eq!(paused.pressed(TrackButton::Rewind), TrackStatus::Paused);
        let resumed = paused.pressed(TrackButton::Resume);
        assert_eq!(resumed, TrackStatus::Playing);
        assert_eq!(resumed.pressed(TrackButton::Rewind), TrackStatus::Playing);
    }

    /// The panel opens with the background loop already going and the
    /// one-shot sequence waiting for its Play, which is what the demo does
    /// once the audio world reports that it can sound.
    #[test]
    fn the_panel_opens_with_only_the_background_playing() {
        assert_eq!(TrackRows::START.background.status, TrackStatus::Playing);
        assert_eq!(TrackRows::START.sequence.status, TrackStatus::Idle);
    }

    /// A track that played to its end puts its row back to the start: the
    /// audio world's count is what carries that, so a row that has not
    /// seen the new one keeps showing the transport it was driving.
    #[test]
    fn a_row_goes_back_to_its_start_when_its_track_ends() {
        let playing = TrackRow {
            status: TrackStatus::Playing,
            ends: 0,
        };
        assert_eq!(playing.observed(0), playing);
        let ended = playing.observed(1);
        assert_eq!(ended.status, TrackStatus::Idle);
        assert_eq!(ended.ends, 1);
        // The row it came back to is the idle one the panel started with:
        // the same row offers the same Play again.
        assert_eq!(ended.status.buttons(), TrackStatus::Idle.buttons());
        // The count only moves forward, so a later frame with the same
        // report leaves the row where it is.
        assert_eq!(ended.observed(1), ended);
    }
}
