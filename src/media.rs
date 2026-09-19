//! Inline images: Yazi capability detection with Ratatui's scrolling image widgets.

use std::collections::BTreeMap;
use std::future::{Future, pending};
use std::io::Write;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, ensure};
use image::{ImageDecoder, ImageReader, Limits};
use ratatui::{
    Frame,
    layout::{Rect, Size},
};
use ratatui_image::{
    FontSize,
    picker::{Picker, ProtocolType},
    sliced::{SignedPosition, SlicedImage, SlicedProtocol},
};
use yazi_adapter::drivers::{Driver, Drivers};
use yazi_emulator::{CLOSE, Dimension, EMULATOR, ESCAPE, Emulator, START};

use crate::app::AppState;
use crate::model::ChatId;

#[path = "../vendor/yazi-image/icc.rs"]
#[allow(clippy::all, clippy::pedantic)]
#[rustfmt::skip]
mod icc;

const IMAGE_ALLOC: u64 = 128 * 1024 * 1024;
const IMAGE_BOUND: u32 = 8192;
static KITTY_USED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaSlot {
    pub chat_id: ChatId,
    pub message_id: i32,
    pub viewport: Rect,
    pub offset: i16,
    pub size: Size,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ImageKey {
    path: PathBuf,
    request_id: u64,
    width: u16,
    height: u16,
    generation: u64,
}

type Encoded = Vec<(ImageKey, Result<SlicedProtocol>)>;
pub type MediaFailure = (ChatId, i32, String);

#[derive(Default)]
pub struct PreviewRenderer {
    targets: Vec<(MediaSlot, ImageKey)>,
    ready: BTreeMap<ImageKey, SlicedProtocol>,
    pending: Option<Pin<Box<dyn Future<Output = Result<Encoded>>>>>,
    generation: u64,
}

impl PreviewRenderer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Layout and encoding are separate: decoding never blocks terminal input.
    /// Only visible images occupy the encoded cache. No second stdin reader.
    pub fn render(&mut self, frame: &mut Frame<'_>, app: &AppState, force: bool) {
        let targets = app
            .media_slots
            .iter()
            .filter_map(|slot| {
                let preview = app.media_previews.get(&(slot.chat_id, slot.message_id))?;
                Some((
                    slot.clone(),
                    ImageKey {
                        path: preview.path.clone()?,
                        request_id: preview.request_id,
                        width: slot.size.width,
                        height: slot.size.height,
                        generation: self.generation,
                    },
                ))
            })
            .collect::<Vec<_>>();
        let changed = self.targets != targets;
        // Releasing Kitty images uses the same deletion sequence as Yazi.
        // Re-encode retained images too, since the library tracks transmission.
        if self
            .ready
            .keys()
            .any(|key| !targets.iter().any(|(_, target)| target == key))
        {
            cleanup();
            self.ready.clear();
        }
        self.targets = targets;
        if changed || force {
            for cell in &mut frame.buffer_mut().content {
                cell.set_diff_option(ratatui::buffer::CellDiffOption::AlwaysUpdate);
            }
        }
        for (slot, key) in &self.targets {
            if let Some(protocol) = self.ready.get(key) {
                if matches!(protocol, SlicedProtocol::Kitty(_)) {
                    KITTY_USED.store(true, Ordering::Release);
                }
                frame.render_widget(
                    SlicedImage::new(protocol, SignedPosition::from((0, slot.offset))),
                    slot.viewport,
                );
            }
        }
        if self.pending.is_none() {
            let keys = self
                .targets
                .iter()
                .map(|(_, key)| key)
                .filter(|key| !self.ready.contains_key(*key))
                .cloned()
                .collect::<Vec<_>>();
            if !keys.is_empty() {
                self.pending = Some(Box::pin(async move {
                    EMULATOR.probe.wait(EMULATOR.probe.id.get()).await;
                    let picker = picker();
                    tokio::task::spawn_blocking(move || {
                        keys.into_iter()
                            .map(|key| {
                                let result = encode(&picker, &key);
                                (key, result)
                            })
                            .collect()
                    })
                    .await
                    .context("image encoding worker failed")
                }));
            }
        }
    }

    /// # Errors
    /// Returns an error if the encoding worker panics.
    pub async fn finished(&mut self) -> Result<Vec<MediaFailure>> {
        let encoded = match self.pending.as_mut() {
            Some(task) => task.await,
            None => pending().await,
        };
        self.pending = None;
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(error) => {
                return Ok(self
                    .targets
                    .iter()
                    .map(|(slot, _)| (slot.chat_id, slot.message_id, format!("{error:#}")))
                    .collect());
            }
        };
        let mut failures = Vec::new();
        for (key, result) in encoded {
            let Some((slot, _)) = self.targets.iter().find(|(_, target)| *target == key) else {
                continue;
            };
            match result {
                Ok(protocol) => {
                    self.ready.insert(key, protocol);
                }
                Err(error) => failures.push((slot.chat_id, slot.message_id, format!("{error:#}"))),
            }
        }
        Ok(failures)
    }

    pub fn invalidate(&mut self) {
        cleanup();
        self.ready.clear();
        self.targets.clear();
        self.generation = self.generation.wrapping_add(1);
    }
}

impl Drop for PreviewRenderer {
    fn drop(&mut self) {
        cleanup();
    }
}

pub fn cleanup() {
    if KITTY_USED.swap(false, Ordering::AcqRel) {
        let _ = Emulator::move_lock((0, 0), |writer| {
            Ok(write!(writer, "{START}_Gq=2,a=d,d=A{ESCAPE}\\{CLOSE}")?)
        });
    }
}

#[allow(deprecated, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn picker() -> Picker {
    let (width, height) = Dimension::cell_size().unwrap_or((10.0, 20.0));
    // from_query_stdio would compete with Yazi's single input reader.
    let mut picker = Picker::from_fontsize(FontSize::new(
        width.clamp(1.0, 1024.0) as u16,
        height.clamp(1.0, 1024.0) as u16,
    ));
    let protocol = match Drivers::matches(&EMULATOR) {
        Driver::Kgp => ProtocolType::Kitty,
        Driver::Iip => ProtocolType::Iterm2,
        Driver::Sixel => ProtocolType::Sixel,
        // Old Kitty implementations lack unicode placeholders needed for scrolling.
        _ => ProtocolType::Halfblocks,
    };
    picker.set_protocol_type(protocol);
    picker
}

fn encode(picker: &Picker, key: &ImageKey) -> Result<SlicedProtocol> {
    let mut reader = ImageReader::open(&key.path)?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(IMAGE_ALLOC);
    limits.max_image_width = Some(IMAGE_BOUND);
    limits.max_image_height = Some(IMAGE_BOUND);
    reader.limits(limits);
    let mut decoder = reader.with_guessed_format()?.into_decoder()?;
    ensure!(
        decoder.total_bytes() <= IMAGE_ALLOC,
        "Image exceeds the 128 MiB decoded size limit"
    );
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = icc::Icc::transform(decoder)?;
    image.apply_orientation(orientation);
    Ok(SlicedProtocol::new(
        picker,
        image,
        Some(Size::new(key.width, key.height)),
    )?)
}
