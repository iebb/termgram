//! Conversation density and background choices, independent of frame rendering.
use crate::appearance::TerminalColor;
use ratatui::style::Color;
use serde::Deserialize;

/// How image attachments appear in the transcript.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Images {
    /// Download and render supported images inline (default).
    #[default]
    Inline,
    /// Show the regular `[photo]`/`[sticker]` placeholder row only; the
    /// preview key still opens the image explicitly.
    Placeholder,
}

impl Images {
    #[must_use]
    pub const fn inline(self) -> bool {
        matches!(self, Self::Inline)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Configuration {
    pub spacing: u16,
    pub alternating: bool,
    /// None blends with the background reported by the existing Yazi reader.
    pub alternate_background: Option<TerminalColor>,
    pub images: Images,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            spacing: 0,
            alternating: true,
            alternate_background: None,
            images: Images::Inline,
        }
    }
}

impl Configuration {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.spacing <= 2,
            "messages.spacing must be between 0 and 2"
        );
        Ok(())
    }

    /// Like Codex's message backgrounds, shade relative to the user's theme.
    /// No terminal reads or palette guesses are made while drawing.
    #[must_use]
    pub fn background(&self, alternate: bool, terminal: Option<[u8; 3]>) -> Color {
        if !self.alternating || !alternate {
            return Color::Reset;
        }
        if let Some(color) = self.alternate_background {
            return color.color();
        }
        let Some(rgb) = terminal else {
            return Color::Reset;
        };
        let light = u32::from(rgb[0]) * 3 + u32::from(rgb[1]) * 6 + u32::from(rgb[2]) > 1_500;
        let shade = rgb.map(|channel| {
            let channel = u16::from(channel);
            u8::try_from(if light {
                channel * 94 / 100
            } else {
                channel + (255 - channel) * 8 / 100
            })
            .unwrap_or_default()
        });
        Color::Rgb(shade[0], shade[1], shade[2])
    }
}
