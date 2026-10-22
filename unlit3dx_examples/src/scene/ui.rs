//! The control panel: the scene's UI, drawn over the frame.
//!
//! The panel lives in the gameplay world with the rest of the scene's objects,
//! and the render world draws it. A [`UiPanel`] can only be built where its
//! egui context runs, so the gameplay world lays the panel out on its own tick
//! and sends the frame to the render world, whose [`UiSource`] holds and draws
//! it.
//!
//! Everything the panel decides is decided here, beside the objects it acts
//! on: the drop and clear buttons call the scene's own request helpers, and the
//! transport buttons write the [`MidiSequencer`] of the track they belong to.
//! The sequences live in this world, so a button is a field write rather than a
//! message to another world, and a row reads the transport state it shows
//! instead of remembering it.

use unlit_ecs::World;
use unlit_ecs::bundle;
use unlit3d::prelude::*;
use unlit3d::ui::{UiPanel, UiSource, egui};
use unlit3dx::prelude::*;

use super::{PanelSlots, SceneTracks, Track, request_clear_in, request_drop_in};

/// What one of the panel's two track rows shows.
///
/// The row derives it from the sequence rather than keeping it: the sequence is
/// a component of this world, so the transport state a row shows is the
/// transport state that drives the music.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackStatus {
    /// Nothing is playing and the sequence stands at its start, so its row
    /// offers a start.
    Idle,
    /// The sequence is playing.
    Playing,
    /// The sequence was paused part-way through.
    Paused,
}

impl TrackStatus {
    /// The state `sequencer` shows.
    fn of(sequencer: &MidiSequencer) -> Self {
        if sequencer.finished {
            return Self::Idle;
        }
        if !sequencer.paused {
            return Self::Playing;
        }
        if sequencer.time == 0.0 && sequencer.index == 0 {
            Self::Idle
        } else {
            Self::Paused
        }
    }

    /// The buttons a row in this state shows, in the order it shows them.
    fn buttons(self) -> &'static [TrackButton] {
        match self {
            Self::Idle => &[TrackButton::Play],
            Self::Playing => &[TrackButton::Pause, TrackButton::Rewind],
            Self::Paused => &[TrackButton::Resume, TrackButton::Rewind],
        }
    }
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

/// One track's transport: its name, then the buttons its state calls for.
///
/// The row names the track so the buttons need no suffix of their own. A
/// sequence that played to its end is idle again, so its row offers the same
/// Play, which returns it to its start before starting it.
fn track_controls(ui: &mut egui::Ui, label: &str, sequencer: &mut MidiSequencer) {
    ui.horizontal(|ui| {
        ui.label(label);
        for button in TrackStatus::of(sequencer).buttons() {
            if !ui.button(button.label()).clicked() {
                continue;
            }
            match button {
                // A sequence that finished is started by rewinding it first:
                // a finished sequencer stays put however far its clock moves.
                TrackButton::Play => {
                    sequencer.rewind();
                    sequencer.paused = false;
                }
                TrackButton::Pause => sequencer.paused = true,
                TrackButton::Resume => sequencer.paused = false,
                TrackButton::Rewind => sequencer.rewind(),
            }
        }
    });
}

/// Mount the panel: one entity carrying the source that runs it.
///
/// The panel reads its input from the gameplay world's own buffer, which
/// the world subscribes when it is set up and dispatches on its tick, so
/// nothing input-related is spawned here: the events the panel reacts to
/// are the ones the rest of this world saw on the same tick.
///
/// The background loop starts itself once the audio world can sound; the
/// one-shot sequence waits for its row's Play button and plays once per
/// press. Each track then pauses, resumes and rewinds on its own, and the
/// shapes drop or clear without touching the music. A scene with no
/// synthesizer has no [`SceneTracks`] and nothing to transport, so its panel
/// shows the shapes alone.
///
/// The panel owns no state: a row reads the sequence it draws, and pressing a
/// button writes it. egui may run a panel more than once in a frame, and every
/// write here is one a second run repeats harmlessly.
pub fn mount_controls(world: &mut World) {
    world
        .root_scope_mut()
        .spawn(bundle![UiPanel::new(|world, _entity, ui| {
            egui::Window::new("Controls").show(ui.ctx(), |ui| {
                // The tracks are spawned once the audio world has answered
                // with its voice, so a scene whose soundfont failed to load
                // shows the shape buttons alone.
                if let Some(tracks) = world.query::<&SceneTracks>().next().map(|tracks| *tracks) {
                    // One row at a time: the borrow each one takes is given
                    // back before the next asks for its own.
                    if let Some(mut track) = world.query_one::<&mut Track>(tracks.background) {
                        track_controls(ui, "Background music", &mut track.midi);
                    }
                    if let Some(mut track) = world.query_one::<&mut Track>(tracks.sequence) {
                        track_controls(ui, "MIDI sequence", &mut track.midi);
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
        }),]);
}

/// A job that gives the render world the source drawing this scene's panel.
///
/// The panel itself is not here: it lives in the gameplay world, which lays it
/// out and sends the frame over. This world owns the slots those frames land
/// in, and measures the target the publisher has to match.
///
/// `pass` is the pass the source draws into: a pass records the sources below
/// it, so a source spawned anywhere else would never reach the frame.
pub fn control_panel_job(panels: PanelSlots, pass: Entity) -> Job<()> {
    Box::new(move |world: &mut World| {
        let mut source = UiSource::new();
        source.publish_target(panels.target);
        world
            .scope_mut(pass)
            .expect("the scene's pass is alive")
            .spawn(bundle![Source::new(source)]);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sequence nothing has been played into: it stands at its start.
    fn sequence() -> MidiSequencer {
        MidiSequencer::new(midi_file(std::iter::empty()).expect("an empty file is well formed"))
    }

    #[test]
    fn a_fresh_sequence_offers_a_start() {
        let mut sequencer = sequence();
        sequencer.paused = true;
        assert_eq!(TrackStatus::of(&sequencer), TrackStatus::Idle);
        assert_eq!(TrackStatus::of(&sequencer).buttons(), &[TrackButton::Play]);
    }

    #[test]
    fn a_running_sequence_offers_a_pause() {
        let sequencer = sequence();
        assert_eq!(TrackStatus::of(&sequencer), TrackStatus::Playing);
        assert_eq!(
            TrackStatus::of(&sequencer).buttons(),
            &[TrackButton::Pause, TrackButton::Rewind]
        );
    }

    #[test]
    fn a_paused_sequence_offers_a_resume() {
        let mut sequencer = sequence();
        sequencer.time = 1.0;
        sequencer.paused = true;
        assert_eq!(TrackStatus::of(&sequencer), TrackStatus::Paused);
        assert_eq!(
            TrackStatus::of(&sequencer).buttons(),
            &[TrackButton::Resume, TrackButton::Rewind]
        );
    }

    #[test]
    fn a_played_out_sequence_offers_a_start_again() {
        let mut sequencer = sequence();
        sequencer.finished = true;
        assert_eq!(TrackStatus::of(&sequencer), TrackStatus::Idle);
        assert_eq!(TrackStatus::of(&sequencer).buttons(), &[TrackButton::Play]);
    }

    #[test]
    fn a_rewind_puts_the_row_back_at_its_start() {
        let mut sequencer = sequence();
        sequencer.time = 1.0;
        sequencer.paused = true;
        sequencer.rewind();
        assert_eq!(TrackStatus::of(&sequencer), TrackStatus::Idle);
    }
}
