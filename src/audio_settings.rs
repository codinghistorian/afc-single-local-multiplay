use bevy::audio::{AudioSink, AudioSinkPlayback, Volume};
use bevy::prelude::*;

use crate::control_settings::{AudioChannel, ControlPreferences};

#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub(crate) struct CategorizedAudioPlayback {
    channel: AudioChannel,
    authored_gain: f32,
}

impl CategorizedAudioPlayback {
    pub(crate) const fn music() -> Self {
        Self {
            channel: AudioChannel::Music,
            authored_gain: 1.0,
        }
    }

    pub(crate) const fn sound_effect(authored_gain: f32) -> Self {
        Self {
            channel: AudioChannel::SoundEffects,
            authored_gain,
        }
    }

    pub(crate) fn output_gain(self, preferences: &ControlPreferences) -> f32 {
        self.authored_gain * preferences.audio(self.channel).effective_gain()
    }
}

pub(crate) fn sync_audio_playback_gains(
    preferences: Res<ControlPreferences>,
    mut playback: Query<(&CategorizedAudioPlayback, &mut AudioSink)>,
) {
    let preferences_changed = preferences.is_changed();
    for (categorized, mut sink) in &mut playback {
        if preferences_changed || sink.is_added() {
            sink.set_volume(Volume::Linear(categorized.output_gain(&preferences)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authored_sound_effect_gain_is_multiplied_by_channel_gain() {
        let mut preferences = ControlPreferences::default();
        *preferences.audio_mut(AudioChannel::SoundEffects) =
            crate::control_settings::AudioChannelPreference::new(true, 50);

        let playback = CategorizedAudioPlayback::sound_effect(0.8);

        assert!((playback.output_gain(&preferences) - 0.2).abs() < f32::EPSILON);
    }
}
