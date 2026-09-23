//! Sticker panel RPCs and thumbnail transfers over raw TL requests.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use grammers_client::Client;
use grammers_client::media::Downloadable;
use grammers_client::tl;

use super::media_cache;
use crate::model::{StickerOverview, StickerRef, StickerSetRef, sanitize_terminal_line};

/// Recent and favorite stickers plus the installed set list carry short-lived
/// file references, so the panel refetches all three on every open.
pub(super) async fn overview(client: &Client) -> Result<StickerOverview> {
    let (recent, favorites, sets) = tokio::try_join!(
        client.invoke(&tl::functions::messages::GetRecentStickers {
            attached: false,
            hash: 0,
        }),
        client.invoke(&tl::functions::messages::GetFavedStickers { hash: 0 }),
        client.invoke(&tl::functions::messages::GetAllStickers { hash: 0 }),
    )?;
    let recent = match recent {
        tl::enums::messages::RecentStickers::Stickers(stickers) => stickers.stickers,
        tl::enums::messages::RecentStickers::NotModified => Vec::new(),
    };
    let favorites = match favorites {
        tl::enums::messages::FavedStickers::Stickers(stickers) => stickers.stickers,
        tl::enums::messages::FavedStickers::NotModified => Vec::new(),
    };
    let sets = match sets {
        tl::enums::messages::AllStickers::Stickers(stickers) => stickers.sets,
        tl::enums::messages::AllStickers::NotModified => Vec::new(),
    };
    Ok(StickerOverview {
        recent: sticker_refs(&recent),
        favorites: sticker_refs(&favorites),
        sets: sets
            .into_iter()
            .filter_map(|set| match set {
                tl::enums::StickerSet::Set(set) if !set.archived => Some(StickerSetRef {
                    id: set.id,
                    access_hash: set.access_hash,
                    title: sanitize_terminal_line(&set.title),
                }),
                tl::enums::StickerSet::Set(_) => None,
            })
            .collect(),
    })
}

pub(super) async fn documents(client: &Client, set: &StickerSetRef) -> Result<Vec<StickerRef>> {
    let result = client
        .invoke(&tl::functions::messages::GetStickerSet {
            stickerset: tl::enums::InputStickerSet::Id(tl::types::InputStickerSetId {
                id: set.id,
                access_hash: set.access_hash,
            }),
            hash: 0,
        })
        .await?;
    let documents = match result {
        tl::enums::messages::StickerSet::Set(loaded) => loaded.documents,
        tl::enums::messages::StickerSet::NotModified => Vec::new(),
    };
    Ok(sticker_refs(&documents))
}

fn sticker_refs(documents: &[tl::enums::Document]) -> Vec<StickerRef> {
    documents.iter().filter_map(sticker_ref).collect()
}

fn sticker_ref(document: &tl::enums::Document) -> Option<StickerRef> {
    let tl::enums::Document::Document(document) = document else {
        return None;
    };
    let emoji = document
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            tl::enums::DocumentAttribute::Sticker(sticker) => Some(sticker.alt.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let thumb_size = document.thumbs.as_ref().and_then(|thumbs| {
        thumbs
            .iter()
            .filter_map(|thumb| match thumb {
                tl::enums::PhotoSize::Size(size) => Some(size),
                _ => None,
            })
            .max_by_key(|size| size.w.saturating_mul(size.h))
            .map(|size| size.r#type.clone())
    });
    Some(StickerRef {
        id: document.id,
        access_hash: document.access_hash,
        file_reference: document.file_reference.clone(),
        emoji: sanitize_terminal_line(&emoji),
        mime_type: sanitize_terminal_line(&document.mime_type),
        thumb_size,
    })
}

struct Thumb {
    id: i64,
    access_hash: i64,
    file_reference: Vec<u8>,
    size: String,
}

impl Downloadable for Thumb {
    fn to_raw_input_location(&self) -> Option<tl::enums::InputFileLocation> {
        Some(
            tl::types::InputDocumentFileLocation {
                id: self.id,
                access_hash: self.access_hash,
                file_reference: self.file_reference.clone(),
                thumb_size: self.size.clone(),
            }
            .into(),
        )
    }
}

/// Sticker thumbnails are immutable per document, so a finished file is reused
/// across panels; the media cache's regular startup prune keeps them bounded.
pub(super) async fn download_thumb(
    client: &Client,
    sticker: &StickerRef,
    directory: PathBuf,
    cache_owner: Option<Arc<std::fs::File>>,
) -> Result<PathBuf> {
    let path = directory.join(format!("sticker_{}.jpg", sticker.id));
    if tokio::fs::try_exists(&path).await? {
        return Ok(path);
    }
    let Some(thumb_size) = sticker.thumb_size.clone() else {
        bail!("Telegram did not provide a static thumbnail for this sticker");
    };
    let thumb = Thumb {
        id: sticker.id,
        access_hash: sticker.access_hash,
        file_reference: sticker.file_reference.clone(),
        size: thumb_size,
    };
    let partial = media_cache::temporary(&directory)?;
    client
        .download_media(&thumb, partial.as_ref() as &Path)
        .await
        .context("Telegram sticker preview transfer failed")?;
    let _owner = cache_owner;
    match partial.persist_noclobber(&path) {
        Ok(()) => Ok(path),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(path),
        Err(error) => Err(error.error).context("could not finish media cache file"),
    }
}
